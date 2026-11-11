pub mod current_conditions;
pub mod forecasts;
pub mod parsers;
pub mod special_weather_statements;

use chrono::{DateTime, FixedOffset, Utc};
use color_eyre::eyre::{eyre, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::environment_canada::weather_feed::WeatherFeed;
use current_conditions::CurrentConditions;
use forecasts::{Builder, Day};
use special_weather_statements::SpecialWeatherStatement;

#[derive(Debug, Serialize, JsonSchema)]
pub struct WeatherReport {
    pub title: String,
    pub updated: DateTime<Utc>,
    pub url: String,
    pub current_conditions: Option<CurrentConditions>,
    pub special_weather_statements: Vec<SpecialWeatherStatement>,
    pub weather_forecasts: Vec<Day>,
}

impl WeatherReport {
    pub fn from_weather_feed(feed: WeatherFeed, tz: CanadaTz) -> Result<WeatherReport> {
        let mut current_conditions = None;
        let mut special_weather_statements = Vec::new();
        let mut forecasts_builder = Builder::new(feed.updated, &tz.chrono());

        for entry in feed.entries {
            match entry.category.term.as_str() {
                "Weather Forecasts" => {
                    forecasts_builder.add(&entry)?;
                }
                "Current Conditions" => {
                    if current_conditions.is_some() {
                        return Err(eyre!(
                            "feed contains multiple entries for \"Current Conditions\""
                        ));
                    } else {
                        current_conditions.replace(CurrentConditions::from_entry(&entry)?);
                    }
                }
                "Warnings and Watches" => {
                    if !entry.title.starts_with("No watches or warnings in effect") {
                        special_weather_statements
                            .push(SpecialWeatherStatement::from_entry(&entry)?);
                    }
                }
                "Notice" => {
                    // Ignore these entries
                }
                term => {
                    return Err(eyre!("feed contains entry with unknown category: {term:?}"));
                }
            }
        }

        let url = feed
            .links
            .iter()
            .find_map(|l| (l.rel.as_deref() == Some("related")).then(|| l.href.clone()))
            .ok_or_else(|| eyre!("missing alternate link"))?;

        Ok(WeatherReport {
            title: feed.title,
            updated: feed.updated,
            url,
            current_conditions,
            special_weather_statements,
            weather_forecasts: forecasts_builder.into_inner(),
        })
    }

    pub fn detected_timezone(&self) -> Option<CanadaTz> {
        self.current_conditions
            .as_ref()
            .map(|cc| cc.observed_at.datetime.tz)
    }
}

/// A timestamp with an associated timezone
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LocalDateTime {
    /// Local ISO 8601 timestamp with a fixed UTC offset
    ts: DateTime<FixedOffset>,
    /// The timezone name
    tz: CanadaTz,
}

/// Canadian timezone (https://www.timeanddate.com/time/zone/canada)
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
pub enum CanadaTz {
    #[serde(rename = "Canada/Atlantic")]
    Atlantic,
    #[serde(rename = "Canada/Central")]
    Central,
    #[serde(rename = "Canada/Eastern")]
    Eastern,
    #[serde(rename = "Canada/Mountain")]
    Mountain,
    #[serde(rename = "Canada/Newfoundland")]
    Newfoundland,
    #[serde(rename = "Canada/Pacific")]
    Pacific,
}

impl CanadaTz {
    fn chrono(&self) -> chrono_tz::Tz {
        match self {
            CanadaTz::Atlantic => chrono_tz::Canada::Atlantic,
            CanadaTz::Central => chrono_tz::Canada::Central,
            CanadaTz::Eastern => chrono_tz::Canada::Eastern,
            CanadaTz::Mountain => chrono_tz::Canada::Mountain,
            CanadaTz::Newfoundland => chrono_tz::Canada::Newfoundland,
            CanadaTz::Pacific => chrono_tz::Canada::Pacific,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_offset_reports_across_local_midnight() {
        for (city, zone, offset_hours, tz) in [
            ("Airdrie", "ABT", 6, CanadaTz::Mountain),
            ("Whitehorse", "YT", 7, CanadaTz::Mountain),
            ("Vancouver", "PCT", 7, CanadaTz::Pacific),
            ("Winnipeg", "MBT", 5, CanadaTz::Central),
            ("Yellowknife", "MDT", 6, CanadaTz::Mountain),
            ("Kenora", "CDT", 5, CanadaTz::Central),
        ] {
            for (updated, time, weekday, day) in [
                (
                    format!("2026-11-11T{:02}:30:00Z", offset_hours - 1),
                    "11:30 PM",
                    "Tuesday",
                    10,
                ),
                (
                    format!("2026-11-11T{offset_hours:02}:30:00Z"),
                    "12:30 AM",
                    "Wednesday",
                    11,
                ),
            ] {
                let xml = format!(
                    r#"<feed xmlns="http://www.w3.org/2005/Atom">
                        <title>{city} - Weather - Environment Canada</title>
                        <updated>{updated}</updated>
                        <link rel="related" href="https://weather.gc.ca/" type="text/html"/>
                        <entry>
                            <id>warning</id><title>Rainfall warning</title>
                            <summary type="html">Heavy rain expected. Issued: {time} {zone} {weekday} {day} November 2026</summary>
                            <link href="https://weather.gc.ca/" type="text/html"/>
                            <category term="Warnings and Watches"/><updated>{updated}</updated>
                        </entry>
                        <entry>
                            <id>conditions</id><title>Current Conditions</title>
                            <summary type="html">&lt;b&gt;Observed at:&lt;/b&gt; {city} Airport {time} {zone} {weekday} {day} November 2026&lt;br/&gt;</summary>
                            <link href="https://weather.gc.ca/" type="text/html"/>
                            <category term="Current Conditions"/><updated>{updated}</updated>
                        </entry>
                        <entry>
                            <id>forecast</id><title>{weekday}: Sunny. High 14.</title>
                            <summary type="html">Sunny. Forecast issued {time} {zone} {weekday} {day} November 2026</summary>
                            <link href="https://weather.gc.ca/" type="text/html"/>
                            <category term="Weather Forecasts"/><updated>{updated}</updated>
                        </entry>
                        </feed>"#
                );
                let feed = WeatherFeed::from_xml_str(&xml).unwrap();
                let report = WeatherReport::from_weather_feed(feed, tz).unwrap();
                assert_eq!(report.special_weather_statements.len(), 1);
                assert_eq!(
                    report.special_weather_statements[0]
                        .issued_at
                        .ts
                        .offset()
                        .local_minus_utc(),
                    -offset_hours * 3600
                );
                assert_eq!(report.special_weather_statements[0].issued_at.tz, tz);
                let conditions = report.current_conditions.unwrap();
                assert_eq!(
                    conditions
                        .observed_at
                        .datetime
                        .ts
                        .offset()
                        .local_minus_utc(),
                    -offset_hours * 3600
                );
                assert_eq!(report.weather_forecasts.len(), 1);
                assert_eq!(
                    report.weather_forecasts[0].date.to_string(),
                    format!("2026-11-{day}")
                );
                let forecast = report.weather_forecasts[0].forecast.forecasts()[0];
                assert_eq!(
                    forecast.issued_at.ts.offset().local_minus_utc(),
                    -offset_hours * 3600
                );
            }
        }
    }
}
