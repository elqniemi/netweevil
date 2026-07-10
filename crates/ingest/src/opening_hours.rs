use anyhow::{Context, Result, bail};
use netweevil_core::{
    EVERY_DAY, FRIDAY, MONDAY, MinuteInterval, PUBLIC_HOLIDAY, SATURDAY, SUNDAY, THURSDAY, TUESDAY,
    TemporalEffect, TemporalRule, TemporalRuleSet, WEDNESDAY,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SourceInterval {
    day_code: String,
    from_time: serde_json::Value,
    to_time: serde_json::Value,
}

/// Parses the generic JSON schedule shape used by mapped GeoPackage
/// `access_schedule` fields. `OPEN` means the edge is closed outside the
/// listed windows; `CLOSE` means it is closed inside the windows.
pub fn parse_access_schedule(details: &str, polarity: &str) -> Result<TemporalRuleSet> {
    let details = details.trim().trim_start_matches('\u{feff}');
    if details.is_empty() || details == "[]" || details.eq_ignore_ascii_case("null") {
        return Ok(TemporalRuleSet::default());
    }
    let source: Vec<SourceInterval> =
        serde_json::from_str(details).context("parsing access schedule JSON")?;
    let effect = if polarity.eq_ignore_ascii_case("OPEN") {
        TemporalEffect::OpenOnly
    } else if polarity.eq_ignore_ascii_case("CLOSE") {
        TemporalEffect::Closed
    } else {
        bail!("access schedule polarity must be OPEN or CLOSE, found '{polarity}'");
    };

    let mut rules = Vec::with_capacity(source.len());
    for (index, interval) in source.into_iter().enumerate() {
        let day_mask = parse_day_code(&interval.day_code)
            .with_context(|| format!("parsing access schedule item {index} day_code"))?;
        let start_minute = parse_hhmm(&interval.from_time, false)
            .with_context(|| format!("parsing access schedule item {index} from_time"))?;
        let end_minute = parse_hhmm(&interval.to_time, true)
            .with_context(|| format!("parsing access schedule item {index} to_time"))?;
        rules.push(TemporalRule {
            day_mask,
            intervals: vec![MinuteInterval {
                start_minute,
                end_minute,
            }],
            effect,
        });
    }
    Ok(TemporalRuleSet { rules })
}

fn parse_day_code(value: &str) -> Result<u8> {
    match value.trim().to_ascii_uppercase().as_str() {
        "ED" | "DAILY" | "EVERYDAY" => Ok(EVERY_DAY),
        "MON" | "MO" => Ok(MONDAY),
        "TUE" | "TU" => Ok(TUESDAY),
        "WED" | "WE" if value.trim().len() >= 3 => Ok(WEDNESDAY),
        "THU" | "TH" => Ok(THURSDAY),
        "FRI" | "FR" => Ok(FRIDAY),
        "SAT" | "SA" => Ok(SATURDAY),
        "SUN" | "SU" => Ok(SUNDAY),
        "WD" | "WEEKDAY" => Ok(MONDAY | TUESDAY | WEDNESDAY | THURSDAY | FRIDAY),
        "WE" | "WEEKEND" => Ok(SATURDAY | SUNDAY),
        "PH" | "PUBLIC_HOLIDAY" => Ok(PUBLIC_HOLIDAY),
        other => bail!("unsupported day code '{other}'"),
    }
}

fn parse_hhmm(value: &serde_json::Value, is_end: bool) -> Result<u16> {
    let raw = match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("HHMM value must be a non-negative integer"))?,
        serde_json::Value::String(value) => value
            .replace(':', "")
            .parse::<u64>()
            .with_context(|| format!("invalid HHMM value '{value}'"))?,
        _ => bail!("HHMM value must be an integer or string"),
    };
    if raw == 2400 {
        return Ok(1440);
    }
    let hour = raw / 100;
    let minute = raw % 100;
    if hour > 23 || minute > 59 {
        bail!("invalid HHMM value {raw}");
    }
    // Government schedule sources commonly encode the inclusive end of a
    // full day as 2359. The runtime uses exclusive interval ends.
    if is_end && raw == 2359 {
        return Ok(1440);
    }
    Ok((hour * 60 + minute) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hhmm_every_day_and_open_polarity() {
        let rules = parse_access_schedule(
            r#"[{"day_code":"ED","from_time":930,"to_time":2359}]"#,
            "OPEN",
        )
        .expect("schedule");
        assert_eq!(rules.rules[0].day_mask, EVERY_DAY);
        assert_eq!(rules.rules[0].intervals[0].start_minute, 9 * 60 + 30);
        assert_eq!(rules.rules[0].intervals[0].end_minute, 1440);
        assert_eq!(rules.rules[0].effect, TemporalEffect::OpenOnly);
    }

    #[test]
    fn preserves_midnight_wrapping_close_intervals() {
        let rules = parse_access_schedule(
            r#"[{"day_code":"FRI","from_time":2200,"to_time":600}]"#,
            "CLOSE",
        )
        .expect("schedule");
        let interval = rules.rules[0].intervals[0];
        assert!(interval.contains(23 * 60));
        assert!(interval.contains(5 * 60));
        assert_eq!(rules.rules[0].effect, TemporalEffect::Closed);
    }

    #[test]
    fn parses_weekday_weekend_and_public_holiday_codes() {
        assert_eq!(parse_day_code("WD").unwrap().count_ones(), 5);
        assert_eq!(parse_day_code("WE").unwrap(), SATURDAY | SUNDAY);
        assert_eq!(parse_day_code("PH").unwrap(), PUBLIC_HOLIDAY);
    }
}
