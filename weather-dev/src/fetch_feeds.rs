use std::collections::HashSet;
use std::path::Path;

use color_eyre::eyre::{eyre, Result};
use reqwest::{Client, Url};
use scraper::{Html, Selector};

const PROVINCE_AND_TERRITORY_CODES: [&str; 13] = [
    "AB", "BC", "MB", "NB", "NL", "NS", "NT", "NU", "ON", "PE", "QC", "SK", "YT",
];

const BASE_URL: &str = "https://weather.gc.ca/forecast/canada/index_e.html";

#[tokio::main]
pub async fn run(cache_dir: &Path) -> Result<()> {
    let client = Client::new();
    let selector =
        Selector::parse("a[href^='/en/location/index.html?coords=']").expect("always valid");
    let base_url = Url::parse(BASE_URL).expect("always valid");

    for province_code in PROVINCE_AND_TERRITORY_CODES {
        let listing_url = province_listing_url(&base_url, province_code);

        let response = client.get(listing_url).send().await?.error_for_status()?;
        let body = response.text().await?;
        let document = Html::parse_document(&body);
        let mut cached_coordinates = HashSet::new();

        for element in document.select(&selector) {
            let href = element
                .value()
                .attr("href")
                .ok_or_else(|| eyre!("location link is missing its href"))?;
            let (latitude, longitude) = coordinates_from_location_href(&base_url, href)?;
            if !cached_coordinates.insert((latitude.clone(), longitude.clone())) {
                continue;
            }
            let feed_url = feed_url(&latitude, &longitude)?;
            let filename = format!("{province_code}-{latitude}_{longitude}.xml");
            let path = cache_dir.join(filename);

            eprintln!("fetching {feed_url}");

            let response = client.get(feed_url).send().await?.error_for_status()?;
            let body = response.text().await?;
            tokio::fs::write(path, body).await?;
        }
    }

    Ok(())
}

fn province_listing_url(base_url: &Url, province_code: &str) -> Url {
    let mut listing_url = base_url.clone();
    listing_url
        .query_pairs_mut()
        .append_pair("id", province_code)
        .append_pair("num_rows", "All");
    listing_url
}

fn coordinates_from_location_href(base_url: &Url, href: &str) -> Result<(String, String)> {
    let location_url = base_url.join(href)?;
    let coordinates = location_url
        .query_pairs()
        .find_map(|(key, value)| (key == "coords").then(|| value.into_owned()))
        .ok_or_else(|| eyre!("location link is missing coordinates: {location_url}"))?;
    let (latitude, longitude) = coordinates
        .split_once(',')
        .ok_or_else(|| eyre!("invalid location coordinates: {coordinates:?}"))?;

    let latitude_value = latitude.parse::<f64>()?;
    let longitude_value = longitude.parse::<f64>()?;
    if !(-90.0..=90.0).contains(&latitude_value) || !(-180.0..=180.0).contains(&longitude_value) {
        return Err(eyre!(
            "location coordinates are out of range: {coordinates:?}"
        ));
    }

    Ok((latitude.to_string(), longitude.to_string()))
}

fn feed_url(latitude: &str, longitude: &str) -> Result<Url> {
    Ok(Url::parse(&format!(
        "https://weather.gc.ca/rss/weather/{latitude}_{longitude}_e.xml"
    ))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_coordinate_based_environment_canada_location_links() {
        let selector =
            Selector::parse("a[href^='/en/location/index.html?coords=']").expect("valid selector");
        let document = Html::parse_fragment(
            r#"<a href="/en/location/index.html?coords=45.529,-73.562">Montréal</a>"#,
        );

        assert_eq!(document.select(&selector).count(), 1);
    }

    #[test]
    fn reads_montreal_coordinates_from_environment_canada_location_link() {
        let base_url = Url::parse(BASE_URL).expect("valid base URL");
        let coordinates = coordinates_from_location_href(
            &base_url,
            "/en/location/index.html?coords=45.529,-73.562",
        )
        .expect("valid Montreal location link");

        assert_eq!(coordinates, ("45.529".to_string(), "-73.562".to_string()));
    }

    #[test]
    fn requests_all_rows_in_the_province_listing() {
        let base_url = Url::parse(BASE_URL).expect("valid base URL");
        let listing_url = province_listing_url(&base_url, "QC");

        assert_eq!(
            listing_url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("id".into(), "QC".into()),
                ("num_rows".into(), "All".into())
            ]
        );
    }

    #[test]
    fn builds_the_environment_canada_feed_url_for_montreal_coordinates() {
        let url = feed_url("45.529", "-73.562").expect("valid Montreal coordinates");

        assert_eq!(
            url.as_str(),
            "https://weather.gc.ca/rss/weather/45.529_-73.562_e.xml"
        );
    }

    #[test]
    fn rejects_invalid_environment_canada_location_coordinates() {
        let base_url = Url::parse(BASE_URL).expect("valid base URL");

        assert!(coordinates_from_location_href(
            &base_url,
            "/en/location/index.html?coords=45.529,west"
        )
        .is_err());
        assert!(coordinates_from_location_href(
            &base_url,
            "/en/location/index.html?coords=91,-73.562"
        )
        .is_err());
    }
}
