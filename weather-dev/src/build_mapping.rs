use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, Write},
    panic,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
};

use color_eyre::eyre::{eyre, Context, Result};
use deunicode::deunicode;
use indoc::{formatdoc, writedoc};
use serde::Deserialize;
use tokio::runtime::{self, Runtime};
use weather_lib::{
    environment_canada::weather_feed::WeatherFeed,
    locations::{Coordinates, OwnedLocation, ProvinceOrTerritory},
    weather_report::{CanadaTz, WeatherReport},
};

use crate::geocode::{self, Geocoder, LonLat};

pub fn run(cache_dir: &Path, mapbox_access_token: Option<&str>) -> Result<()> {
    MappingBuilder::new(cache_dir, mapbox_access_token)?.run()
}

struct SlugRegistry {
    slugs: HashMap<ProvinceOrTerritory, HashSet<String>>,
}

impl SlugRegistry {
    pub fn new() -> Self {
        Self {
            slugs: Default::default(),
        }
    }

    pub fn register(&mut self, province_code: ProvinceOrTerritory, slug: &str) -> Result<()> {
        let province_set = self.slugs.entry(province_code).or_default();

        if province_set.contains(slug) {
            return Err(eyre!("slug already exists: {province_code:?} {slug:?}"));
        }

        province_set.insert(slug.to_string());

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct MissingTimezone(ProvinceOrTerritory, &'static str, CanadaTz);

static MISSING_TIMEZONES: OnceLock<Vec<MissingTimezone>> = OnceLock::new();

// These locations do not provide current conditions which means we can't
// infer a local timezone as forecasts aren't necessarily issued from the
// same timezone.
fn missing_timezones() -> &'static Vec<MissingTimezone> {
    MISSING_TIMEZONES.get_or_init(|| {
        serde_json::from_str(include_str!("../data/missing_timezones.json"))
            .expect("failed to deserialize missing timezones")
    })
}

fn git_project_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("git command failed: {:?}", output.stderr));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let path = stdout
        .lines()
        .next()
        .ok_or_else(|| eyre!("expected git stdout to contain at least one line"))?;

    Ok(PathBuf::from(path))
}

struct MappingBuilder {
    project_root: PathBuf,
    cache_dir: PathBuf,
    slugs: SlugRegistry,
    geocoder: Option<Geocoder>,
    newly_geocoded: HashMap<String, LonLat>,
    tokio_rt: Runtime,
}

impl MappingBuilder {
    fn new<P: Into<PathBuf>>(cache_dir: P, mapbox_access_token: Option<&str>) -> Result<Self> {
        Ok(Self {
            project_root: git_project_root()?,
            cache_dir: cache_dir.into(),
            slugs: SlugRegistry::new(),
            geocoder: mapbox_access_token.map(Geocoder::new),
            newly_geocoded: Default::default(),
            tokio_rt: runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        })
    }

    fn source_file(&self) -> PathBuf {
        let mut path = self.project_root.clone();
        path.extend(&["weather-lib", "src", "locations", "data.rs"]);
        path
    }

    fn coordinates_file(&self) -> PathBuf {
        let mut path = self.project_root.clone();
        path.extend(&["weather-dev", "data", "coordinates.json"]);
        path
    }

    pub fn run(mut self) -> Result<()> {
        let mut locations = Vec::new();

        for file_result in std::fs::read_dir(&self.cache_dir)? {
            let file = file_result?;
            match self
                .handle_file(&file.path())
                .wrap_err_with(|| format!("{:?}", file.path()))
            {
                Ok(location) => {
                    locations.push(location);
                }
                Err(err) => {
                    eprintln!("{:#?}", err);
                }
            }
        }

        let mut rustfmt = Command::new("rustfmt")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let mut stdin = rustfmt.stdin.take().expect("failed to get stdin");
        let writer_thread = thread::spawn(move || write_data_file(&mut stdin, locations));

        let output = rustfmt.wait_with_output()?;

        match writer_thread.join() {
            Ok(result) => result?,
            Err(panic_payload) => {
                panic::resume_unwind(panic_payload);
            }
        }

        if !output.status.success() {
            return Err(eyre!("rustfmt failed"));
        }

        let source_file = self.source_file();

        eprintln!("writing to {:?}", source_file);

        fs::write(source_file, output.stdout)?;

        if !self.newly_geocoded.is_empty() {
            let coordinates_file = self.coordinates_file();

            eprintln!("writing newly-geocoded coordinates to {coordinates_file:?}");

            let mut current: BTreeMap<String, LonLat> =
                serde_json::from_str(&fs::read_to_string(&coordinates_file)?)?;

            current.extend(self.newly_geocoded.drain());

            fs::write(&coordinates_file, serde_json::to_string_pretty(&current)?)?;
        }

        Ok(())
    }

    fn handle_file(&mut self, path: &Path) -> Result<OwnedLocation> {
        let mut reader = BufReader::new(File::open(path)?);

        let feed = WeatherFeed::from_xml_reader(&mut reader)?;

        let feed_url = feed
            .links
            .iter()
            .find_map(|l| (l.rel.as_deref() == Some("self")).then(|| l.href.clone()))
            .ok_or_else(|| eyre!("could not find self link"))?;

        let html_url = feed
            .links
            .iter()
            .find_map(|l| (l.rel.as_deref() == Some("related")).then(|| l.href.clone()))
            .ok_or_else(|| eyre!("could not find related link"))?;

        let province_or_territory = path
            .file_stem()
            .and_then(|os_str| os_str.to_str())
            .and_then(|s| s.split_once('-'))
            .and_then(|(abbr, _)| ProvinceOrTerritory::from_abbr(abbr))
            .ok_or_else(|| eyre!("failed to extract province code and id from file name"))?;

        let name = feed
            .title
            .trim_end_matches(" - Weather - Environment Canada")
            .to_string();

        let slug = slug(&name);
        self.slugs.register(province_or_territory, &slug)?;

        // Note that dates in this report with be invalid as using Canada/Eastern
        // as a placeholder. This is fine as we aren't actually using the forecasts.
        let report = WeatherReport::from_weather_feed(feed, CanadaTz::Eastern)?;

        let tz = match report.detected_timezone() {
            Some(tz) => tz,
            None => {
                let fallback = missing_timezones().iter().find_map(
                    |MissingTimezone(candidate_province, candidate_name, tz)| {
                        (province_or_territory == *candidate_province && &name == candidate_name)
                            .then_some(*tz)
                    },
                );

                fallback.ok_or_else(|| {
                    eyre!("could not find time zone for {name} ({province_or_territory:?}) {html_url}")
                })?
            }
        };

        let search_string = format!(
            "{} {}",
            name,
            province_or_territory.to_abbr().to_ascii_uppercase()
        );

        let LonLat(lon, lat) = match coordinates_from_page(&html_url)?
            .or_else(|| geocode::cache().get(search_string.as_str()).copied())
        {
            Some(entry) => entry,
            None => {
                if let Some(geocoder) = self.geocoder.as_ref() {
                    let response = self.tokio_rt.block_on(geocoder.lookup(&search_string))?;
                    let lonlat = response
                        .features
                        .first()
                        .map(|f| f.center)
                        .ok_or_else(|| eyre!("failed to geocode {search_string:?}"))?;

                    self.newly_geocoded.insert(search_string.clone(), lonlat);

                    lonlat
                } else {
                    return Err(eyre!("could not lookup coordinates for {search_string:?} as none were found in cache and a mapbox access token was not provided"));
                }
            }
        };

        Ok(OwnedLocation {
            name,
            province_or_territory,
            slug,
            tz,
            feed_url,
            html_url,
            coordinates: Coordinates { lat, lon },
        })
    }
}

fn coordinates_from_page(html_url: &str) -> Result<Option<LonLat>> {
    let url = reqwest::Url::parse(html_url)?;
    if !url.query_pairs().any(|(key, _)| key == "coords") {
        return Ok(None);
    }
    let (lat, lon) = crate::fetch_feeds::coordinates_from_location_href(&url, html_url)?;
    Ok(Some(LonLat(lon.parse()?, lat.parse()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_authoritative_page_coordinates() {
        let LonLat(lon, lat) = coordinates_from_page(
            "https://weather.gc.ca/en/location/index.html?coords=76.417,-82.894",
        )
        .unwrap()
        .unwrap();
        assert_eq!((lat, lon), (76.417, -82.894));
    }

    #[test]
    fn only_legacy_pages_need_geocoding() {
        assert!(
            coordinates_from_page("https://weather.gc.ca/city/pages/nu-12_metric_e.html")
                .unwrap()
                .is_none()
        );
        for coords in ["", "NaN,0", "91,0", "0,181", "0", "0,infinity"] {
            assert!(coordinates_from_page(&format!(
                "https://weather.gc.ca/en/location/index.html?coords={coords}"
            ))
            .is_err());
        }
    }
}

fn write_data_file<W>(mut w: W, mut locations: Vec<OwnedLocation>) -> Result<()>
where
    W: Write,
{
    locations.sort_by_cached_key(|l| (l.province_or_territory, l.name.clone()));

    writedoc!(
        w,
        r#"
            //! In-memory listing of all locations provided by Environment Canada.
            //! This file is automatically generated using `weather-dev build-mapping`.
            //! DO NOT EDIT MANUALLY.

            use super::{{Location, ProvinceOrTerritory, Coordinates}};
            use crate::weather_report::CanadaTz;

            pub static LOCATIONS: [Location;{}] = [
       "#,
        locations.len()
    )?;

    for location in &locations {
        writeln!(w, "{},", location_literal(location))?;
    }

    writeln!(w, "];\n")?;

    Ok(())
}

fn location_literal(location: &OwnedLocation) -> String {
    let OwnedLocation {
        province_or_territory,
        slug,
        name,
        tz,
        feed_url,
        html_url,
        coordinates: Coordinates { lat, lon },
    } = location;

    formatdoc! {r#"
        Location {{
            name: {name:?},
            province_or_territory: ProvinceOrTerritory::{province_or_territory:?},
            slug: {slug:?},
            tz: CanadaTz::{tz:?},
            feed_url: {feed_url:?},
            html_url: {html_url:?},
            coordinates: Coordinates {{ lat: {lat}, lon: {lon} }},
        }}
    "#}
}

fn adjust_name(input: &str) -> String {
    let Some((main, mut alternate)) = input.split_once(" (") else {
        return input.to_string();
    };

    alternate = alternate.trim_end_matches(')');

    match alternate {
        "University of" | "Réserve faunique" | "Parc national" | "Réservoir" => {
            format!("{alternate} {main}")
        }
        "Sanctuary" | "Provincial Park" | "National Park" | "Highway" => {
            format!("{main} {alternate}")
        }
        _ => {
            format!("{main} {alternate}")
        }
    }
}

fn slug(input: &str) -> String {
    let adjusted = adjust_name(input);
    let ascii = deunicode(&adjusted);
    let mut slug = String::with_capacity(ascii.len());

    for c in ascii.chars() {
        if c.is_ascii_alphabetic() {
            slug.push(c.to_ascii_lowercase())
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }

    slug
}
