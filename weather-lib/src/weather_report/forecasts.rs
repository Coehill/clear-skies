use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc, Weekday};
use color_eyre::eyre::{eyre, Result};
use schemars::JsonSchema;
use serde::Serialize;

use crate::environment_canada::weather_feed::Entry;

use super::parsers::conditions::Condition;
use super::{parsers, LocalDateTime};

pub struct Builder<Tz: TimeZone> {
    forecasts: Vec<Day>,
    local_time: DateTime<Tz>,
    local_date: Option<NaiveDate>,
}

impl<Tz: TimeZone> Builder<Tz> {
    pub fn new(feed_updated: DateTime<Utc>, local_tz: &Tz) -> Self {
        Self {
            forecasts: Vec::with_capacity(14),
            local_time: feed_updated.with_timezone(local_tz),
            local_date: None,
        }
    }

    pub fn add(&mut self, entry: &Entry) -> Result<()> {
        let (forecast, period) = Forecast::from_entry(entry)?;
        let local_date = *self.local_date.get_or_insert_with(|| {
            self.local_time
                .with_timezone(forecast.issued_at.ts.offset())
                .date_naive()
        });

        if period.is_night {
            match self.forecasts.last_mut() {
                // The last forecast should be for the same day
                Some(last_day) => {
                    if period.weekday != last_day.date.weekday() {
                        return Err(eyre!("entry weekday mismatch: {:?}", &entry.title));
                    }

                    last_day.make_detailed(forecast)?;
                }
                // Feed was retrieved during the day, so only the night forecast is available
                None => {
                    self.forecasts.push(Day {
                        date: local_date,
                        forecast: DailyForecast::Night(forecast),
                    });

                    self.local_date = Some(local_date + Days::new(1));
                }
            }
        } else {
            if period.weekday != local_date.weekday() {
                return Err(eyre!(
                    "expected {} for entry: {:?}",
                    local_date.weekday(),
                    &entry.title
                ));
            }

            self.forecasts.push(Day {
                date: local_date,
                forecast: DailyForecast::Abridged(forecast),
            });

            self.local_date = Some(local_date + Days::new(1));
        }

        Ok(())
    }

    pub fn into_inner(self) -> Vec<Day> {
        self.forecasts
    }
}

/// Weather forecast(s) for a single calendar day
#[derive(Debug, Serialize, JsonSchema)]
pub struct Day {
    pub date: NaiveDate,
    pub forecast: DailyForecast,
}

impl Day {
    fn make_detailed(&mut self, night: Forecast) -> Result<()> {
        let DailyForecast::Abridged(ref day) = self.forecast else {
            return Err(eyre!("forecast is already detailed"));
        };

        self.forecast = DailyForecast::Detailed {
            day: day.clone(),
            night,
        };

        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "scope", content = "content")]
pub enum DailyForecast {
    /// Evening forecast for the current day
    Night(Forecast),
    /// Separate day and night forecasts (usually issued for the next 5 days)
    Detailed { day: Forecast, night: Forecast },
    /// Single forecast covering the entire day (usually issued beyond 5 days out)
    Abridged(Forecast),
}

impl DailyForecast {
    pub fn forecasts(&self) -> Vec<&Forecast> {
        match self {
            DailyForecast::Night(forecast) => vec![forecast],
            DailyForecast::Detailed { day, night } => vec![day, night],
            DailyForecast::Abridged(forecast) => vec![forecast],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Forecast {
    /// A short description of forecasted weather condition (e.g. "Chance of showers")
    pub condition: String,
    /// Best-effort attempt to convert the provided conditions into something that can be used to show an icon.
    pub normalized_condition: Option<Vec<Condition>>,
    /// The [probability of precipitation](https://www.canada.ca/en/environment-climate-change/services/sky-watchers/glossary.html#wsDTE9CAF366) as a percentage
    pub probability_of_precipitation: Option<u8>,
    /// The forecasted temperature
    pub temperature: Temperature,
    /// A longer description of forecasted weather conditions (e.g. "Cloudy with 70 percent chance of showers. Low 17.")
    pub summary: String,
    /// When the forecast was issued
    pub issued_at: LocalDateTime,
}

struct ForecastPeriod {
    weekday: Weekday,
    is_night: bool,
}

impl Forecast {
    fn from_entry(entry: &Entry) -> Result<(Self, ForecastPeriod)> {
        let (_, parsed_title) = parsers::title::parse(&entry.title)?;

        let summary_html = scraper::Html::parse_fragment(&entry.summary.content);
        let full_summary = summary_html.root_element().text().collect::<String>();
        let (_, (summary, issued_at)) = parsers::forecast_summary(&full_summary)?;

        let normalized_condition = parsers::conditions::parse(&parsed_title.condition)
            .map(|(_, parsed)| parsed)
            .ok();

        Ok((
            Forecast {
                condition: parsed_title.condition,
                normalized_condition,
                probability_of_precipitation: parsed_title.probability_of_precipitation,
                temperature: parsed_title.temperature,
                summary: summary.to_string(),
                issued_at,
            },
            ForecastPeriod {
                weekday: parsed_title.weekday,
                is_night: parsed_title.is_night,
            },
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct Temperature {
    pub degrees_c: f32,
    pub trend: TemperatureTrend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureTrend {
    High,
    Low,
    Steady,
}
