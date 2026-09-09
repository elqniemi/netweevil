use anyhow::{Context, Result, bail};
use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::model::{TransitBundle, TransitTimeContext};

/// GTFS times are elapsed seconds from local noon minus twelve hours, not
/// wall-clock times on a daylight-saving transition day.
pub(crate) fn service_time_basis(
    agency_timezone: &str,
    service_dates: &[time::Date],
) -> Result<(i64, Vec<u32>)> {
    let timezone: Tz = agency_timezone.parse().context("invalid agency_timezone")?;
    let mut anchors = Vec::with_capacity(service_dates.len());
    let mut first_midnight = None;
    for (index, raw_date) in service_dates.iter().enumerate() {
        let date = NaiveDate::from_ymd_opt(
            raw_date.year(),
            raw_date.month() as u32,
            u32::from(raw_date.day()),
        )
        .context("invalid GTFS service date")?;
        let noon = date.and_hms_opt(12, 0, 0).expect("valid noon");
        let noon = timezone
            .from_local_datetime(&noon)
            .single()
            .with_context(|| {
                format!("service date {raw_date} has no unique local noon in {agency_timezone}")
            })?;
        let anchor = noon.timestamp() - 12 * 3600;
        anchors.push(anchor);
        if index == 0 {
            // On an autumn transition GTFS zero may be after midnight. Keep
            // early-morning queries representable before the first service.
            first_midnight = timezone
                .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("valid midnight"))
                .earliest()
                .map(|midnight| midnight.timestamp());
        }
    }
    let first_anchor = *anchors.first().context("transit service window is empty")?;
    let origin = first_midnight.map_or(first_anchor, |midnight| midnight.min(first_anchor));
    let offsets = anchors
        .into_iter()
        .map(|anchor| {
            u32::try_from(anchor - origin)
                .context("transit service window exceeds supported time range")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((origin, offsets))
}

pub(crate) fn request_time_seconds(bundle: &TransitBundle, raw: &str) -> Result<u32> {
    let unix_s = if let Ok(datetime) = DateTime::parse_from_rfc3339(raw) {
        if datetime.nanosecond() != 0 {
            bail!("transit datetime must use whole-second precision");
        }
        datetime.timestamp()
    } else {
        let naive = parse_naive_datetime(raw)?;
        let timezone: Tz = bundle
            .agency_timezone
            .parse()
            .context("invalid agency_timezone")?;
        match timezone.from_local_datetime(&naive) {
            LocalResult::Single(datetime) => datetime.timestamp(),
            LocalResult::Ambiguous(_, _) => bail!(
                "transit datetime '{raw}' is ambiguous in {}; supply an explicit UTC offset",
                bundle.agency_timezone,
            ),
            LocalResult::None => bail!(
                "transit datetime '{raw}' does not exist in {} because of a clock change",
                bundle.agency_timezone,
            ),
        }
    };
    u32::try_from(unix_s - bundle.time_origin_unix_s).with_context(|| {
        format!("transit datetime '{raw}' precedes the timetable origin or exceeds its supported time range")
    })
}

fn parse_naive_datetime(raw: &str) -> Result<NaiveDateTime> {
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).expect("valid midnight"));
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(raw, format) {
            return Ok(datetime);
        }
    }
    bail!(
        "invalid transit datetime '{raw}'; use RFC3339 with an offset or a local YYYY-MM-DDTHH:MM:SS datetime"
    )
}

impl TransitBundle {
    pub fn time_context(&self) -> TransitTimeContext {
        TransitTimeContext {
            agency_timezone: self.agency_timezone.clone(),
            time_origin_unix_s: self.time_origin_unix_s,
        }
    }
}

impl TransitTimeContext {
    /// Formats a returned departure_s/arrival_s as an agency-local RFC3339
    /// datetime, preserving the UTC offset that distinguishes repeated hours.
    pub fn datetime(&self, seconds: u32) -> Result<String> {
        let timezone: Tz = self
            .agency_timezone
            .parse()
            .context("invalid agency_timezone")?;
        let unix_s = self
            .time_origin_unix_s
            .checked_add(i64::from(seconds))
            .context("transit datetime exceeds supported time range")?;
        let datetime = timezone
            .timestamp_opt(unix_s, 0)
            .single()
            .context("transit datetime exceeds supported time range")?;
        Ok(datetime.to_rfc3339())
    }
}
