use std::{collections::HashMap, sync::OnceLock};

use color_eyre::eyre::{eyre, Result};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};

pub struct Geocoder {
    mapbox_access_token: String,
    http_client: reqwest::Client,
}

static CACHE: OnceLock<HashMap<&'static str, LonLat>> = OnceLock::new();

pub fn cache() -> &'static HashMap<&'static str, LonLat> {
    CACHE.get_or_init(|| serde_json::from_str(include_str!("../data/coordinates.json")).unwrap())
}

impl Geocoder {
    const BASE_URL: &'static str = "https://api.mapbox.com/geocoding/v5";

    pub fn new<S: ToString>(mapbox_access_token: S) -> Self {
        Self {
            mapbox_access_token: mapbox_access_token.to_string(),
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn lookup(&self, search_text: &str) -> Result<GeocoderResponse> {
        let mut url = Url::parse(Self::BASE_URL).unwrap();

        url.path_segments_mut()
            .unwrap()
            .extend(&["mapbox.places", &format!("{search_text}.json")]);

        url.query_pairs_mut().extend_pairs(&[
            ("country", "ca"),
            ("fuzzyMatch", "false"),
            ("language", "en"),
            ("access_token", &self.mapbox_access_token),
        ]);

        let response = self.http_client.get(url).send().await?;

        if response.status() != StatusCode::OK {
            let status = response.status();
            let body = response.text().await?;

            return Err(eyre!(
                "Mapbox API returned status code {status} with body {body}"
            ));
        }

        let body: GeocoderResponse = response.json().await?;

        Ok(body)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct GeocoderResponse {
    pub features: Vec<Feature>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Feature {
    #[allow(unused)]
    pub place_name: String,
    pub center: LonLat,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
pub struct LonLat(pub f64, pub f64);

#[cfg(test)]
mod tests {
    use super::*;
    use weather_lib::locations::{Location, ProvinceOrTerritory};
    use weather_lib::weather_report::CanadaTz;

    #[test]
    fn catalogue_coordinates_agree_with_cache_and_urls() {
        for location in &weather_lib::locations::data::LOCATIONS {
            let key = format!(
                "{} {}",
                location.name,
                location
                    .province_or_territory
                    .to_abbr()
                    .to_ascii_uppercase()
            );
            let LonLat(lon, lat) = cache().get(key.as_str()).unwrap();
            assert_eq!(
                (location.coordinates.lat, location.coordinates.lon),
                (*lat, *lon),
                "{key}"
            );
            let page = Url::parse(location.html_url).unwrap();
            let (page_lat, page_lon) =
                crate::fetch_feeds::coordinates_from_location_href(&page, location.html_url)
                    .unwrap();
            assert_eq!(
                (
                    page_lat.parse::<f64>().unwrap(),
                    page_lon.parse::<f64>().unwrap()
                ),
                (*lat, *lon),
                "{key}"
            );
            assert_eq!(
                location.feed_url,
                format!("https://weather.gc.ca/rss/weather/{page_lat}_{page_lon}_e.xml"),
                "{key}"
            );
        }
    }

    #[test]
    fn corrected_coordinates_match_catalogue_and_feed_urls() {
        // ECCC province listings: /forecast/canada/index_e.html?id={QC,NU,NT}&num_rows=All
        for (name, province, slug, lat, lon, tz) in [
            (
                "Akulivik",
                ProvinceOrTerritory::Quebec,
                "akulivik",
                60.81,
                -78.188,
                CanadaTz::Eastern,
            ),
            (
                "Arctic Bay",
                ProvinceOrTerritory::Nunavut,
                "arctic-bay",
                73.037,
                -85.152,
                CanadaTz::Central,
            ),
            (
                "Grise Fiord",
                ProvinceOrTerritory::Nunavut,
                "grise-fiord",
                76.417,
                -82.894,
                CanadaTz::Eastern,
            ),
            (
                "Tuktoyaktuk",
                ProvinceOrTerritory::NorthwestTerritories,
                "tuktoyaktuk",
                69.444,
                -133.025,
                CanadaTz::Mountain,
            ),
            (
                "Sambaa K'e",
                ProvinceOrTerritory::NorthwestTerritories,
                "sambaa-k-e",
                60.443,
                -121.242,
                CanadaTz::Mountain,
            ),
            (
                "Kimmirut",
                ProvinceOrTerritory::Nunavut,
                "kimmirut",
                62.847,
                -69.872,
                CanadaTz::Eastern,
            ),
            (
                "Whale Cove",
                ProvinceOrTerritory::Nunavut,
                "whale-cove",
                62.174,
                -92.579,
                CanadaTz::Central,
            ),
            (
                "Saint-Nicéphore",
                ProvinceOrTerritory::Quebec,
                "saint-nicephore",
                45.831,
                -72.419,
                CanadaTz::Eastern,
            ),
        ] {
            let key = format!("{name} {}", province.to_abbr().to_ascii_uppercase());
            let LonLat(cached_lon, cached_lat) = cache().get(key.as_str()).unwrap();
            assert_eq!((*cached_lat, *cached_lon), (lat, lon), "{name}");
            let location = Location::lookup(province, slug).unwrap();
            assert_eq!(location.name, name);
            assert_eq!(location.tz, tz);
            assert_eq!(
                (location.coordinates.lat, location.coordinates.lon),
                (lat, lon)
            );
            assert_eq!(
                location.feed_url,
                format!("https://weather.gc.ca/rss/weather/{lat}_{lon}_e.xml")
            );
            assert_eq!(
                location.html_url,
                format!("https://weather.gc.ca/en/location/index.html?coords={lat},{lon}")
            );
        }
    }
}
