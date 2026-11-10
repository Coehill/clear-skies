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
    let base_url = Url::parse(BASE_URL).expect("always valid");
    let feed_base_url = Url::parse("https://weather.gc.ca/rss/weather/").expect("always valid");

    for province_code in PROVINCE_AND_TERRITORY_CODES {
        cache_province(&client, cache_dir, &base_url, &feed_base_url, province_code).await?;
    }

    Ok(())
}

async fn cache_province(
    client: &Client,
    cache_dir: &Path,
    base_url: &Url,
    feed_base_url: &Url,
    province_code: &str,
) -> Result<()> {
    let selector =
        Selector::parse("a[href^='/en/location/index.html?coords=']").expect("always valid");

    let listing_url = province_listing_url(base_url, province_code);

    let response = client.get(listing_url).send().await?.error_for_status()?;
    let body = response.text().await?;
    let document = Html::parse_document(&body);
    let mut cached_coordinates = HashSet::new();

    for element in document.select(&selector) {
        let href = element
            .value()
            .attr("href")
            .ok_or_else(|| eyre!("location link is missing its href"))?;
        let (latitude, longitude) = coordinates_from_location_href(base_url, href)?;
        if !cached_coordinates.insert((latitude.clone(), longitude.clone())) {
            continue;
        }
        let feed_url = feed_url(feed_base_url, &latitude, &longitude)?;
        let filename = format!("{province_code}-{latitude}_{longitude}.xml");
        let path = cache_dir.join(filename);

        eprintln!("fetching {feed_url}");

        let response = client.get(feed_url).send().await?.error_for_status()?;
        let body = response.text().await?;
        tokio::fs::write(path, body).await?;
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

fn feed_url(base_url: &Url, latitude: &str, longitude: &str) -> Result<Url> {
    Ok(base_url.join(&format!("{latitude}_{longitude}_e.xml"))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    const LOCATION_LINK: &str =
        r#"<a href="/en/location/index.html?coords=45.529,-73.562">Montreal</a>"#;
    const FEED: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom">
        <title>Montreal - Weather - Environment Canada</title>
        <updated>2026-11-10T12:00:00Z</updated>
        <link rel="self" href="https://weather.gc.ca/rss/weather/45.529_-73.562_e.xml"
              type="application/atom+xml"/>
    </feed>"#;

    struct CacheDir(PathBuf);

    impl CacheDir {
        fn new() -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "clear-skies-feeds-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for CacheDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove test cache directory");
        }
    }

    fn mock_server(responses: Vec<(u16, String)>) -> (Url, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "timed out waiting for request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept request: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(line.trim_end().to_string());
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        (base_url, handle)
    }

    async fn download(
        cache: &CacheDir,
        responses: Vec<(u16, String)>,
    ) -> (Result<()>, Vec<String>) {
        let (base_url, server) = mock_server(responses);
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let result = cache_province(
            &client,
            &cache.0,
            &base_url.join("listing").unwrap(),
            &base_url.join("rss/weather/").unwrap(),
            "QC",
        )
        .await;
        (result, server.join().unwrap())
    }

    #[tokio::test]
    async fn downloads_duplicate_location_links_only_once() {
        let cache = CacheDir::new();
        let (result, requests) = download(
            &cache,
            vec![(200, LOCATION_LINK.repeat(2)), (200, FEED.to_string())],
        )
        .await;

        result.unwrap();
        assert_eq!(
            requests,
            [
                "GET /listing?id=QC&num_rows=All HTTP/1.1",
                "GET /rss/weather/45.529_-73.562_e.xml HTTP/1.1",
            ]
        );
        assert_eq!(std::fs::read_dir(&cache.0).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(cache.0.join("QC-45.529_-73.562.xml")).unwrap(),
            FEED
        );
    }

    #[tokio::test]
    async fn returns_listing_http_errors_without_writing_files() {
        let cache = CacheDir::new();
        let (result, requests) = download(&cache, vec![(500, "listing error".into())]).await;

        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(std::fs::read_dir(&cache.0).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn returns_feed_http_errors_without_overwriting_cached_files() {
        let cache = CacheDir::new();
        let path = cache.0.join("QC-45.529_-73.562.xml");
        std::fs::write(&path, FEED).unwrap();
        let (result, requests) = download(
            &cache,
            vec![(200, LOCATION_LINK.into()), (404, "feed error".into())],
        )
        .await;

        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::NOT_FOUND)
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(std::fs::read_to_string(path).unwrap(), FEED);
    }

    #[tokio::test]
    async fn caches_successful_bodies_without_validating_feed_xml() {
        let cache = CacheDir::new();
        let invalid_feed = "<html>Not a weather feed</html>";
        let (result, _) = download(
            &cache,
            vec![(200, LOCATION_LINK.into()), (200, invalid_feed.into())],
        )
        .await;

        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.0.join("QC-45.529_-73.562.xml")).unwrap(),
            invalid_feed
        );
        assert!(
            weather_lib::environment_canada::weather_feed::WeatherFeed::from_xml_str(invalid_feed)
                .is_err()
        );
    }

    #[test]
    fn accepts_coordinate_boundaries() {
        let base_url = Url::parse(BASE_URL).unwrap();
        for coordinates in ["-90,-180", "90,180", "0,0"] {
            let href = format!("/en/location/index.html?coords={coordinates}");
            let (latitude, longitude) = coordinates_from_location_href(&base_url, &href).unwrap();
            assert_eq!(format!("{latitude},{longitude}"), coordinates);
        }
    }

    #[test]
    fn rejects_missing_malformed_and_non_finite_coordinates() {
        let base_url = Url::parse(BASE_URL).unwrap();
        for query in [
            "",
            "coords=",
            "coords=45.529",
            "coords=,",
            "coords=45.529,",
            "coords=,-73.562",
            "coords=45,73,2",
            "coords=NaN,-73",
            "coords=45,NaN",
            "coords=inf,-73",
            "coords=45,-inf",
            "coords=-90.001,0",
            "coords=90.001,0",
            "coords=0,-180.001",
            "coords=0,180.001",
        ] {
            let href = format!("/en/location/index.html?{query}");
            assert!(
                coordinates_from_location_href(&base_url, &href).is_err(),
                "accepted invalid query: {query}"
            );
        }
    }

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
        let base_url = Url::parse("https://weather.gc.ca/rss/weather/").unwrap();
        let url = feed_url(&base_url, "45.529", "-73.562").expect("valid Montreal coordinates");

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
