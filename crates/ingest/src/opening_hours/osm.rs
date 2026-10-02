//! The subset of the OSM `opening_hours` syntax that conditional
//! restrictions use, mapped onto [`TemporalRule`]s.

use netweevil_core::{
    CalendarDate, DateRange, EVERY_DAY, FRIDAY, MONDAY, MinuteInterval, PUBLIC_HOLIDAY, SATURDAY,
    SUNDAY, THURSDAY, TUESDAY, TemporalEffect, TemporalRule, WEDNESDAY,
};

/// Parses an OSM time domain (`Mo-Fr 07:00-19:00; Sa 10:00-12:00`,
/// `Nov 1-Apr 1`, `2025 Jan 13-2025 Apr 4`, `PH off`, `24/7`) into rules
/// that are active exactly when the domain holds. Every rule carries
/// `effect` and applies to every mode; callers scope them.
///
/// Later `;` rules replace earlier ones on the days they name, as in
/// `opening_hours`; `,`-joined additional rules only add. Domains that
/// mention `PH` follow the holiday calendar; others match listed holidays by
/// weekday. Anything outside the subset (sun-relative times, school
/// holidays, nth weekdays, comments, open-ended times) is an error, so
/// callers never import a partial approximation.
pub(crate) fn parse_osm_time_domain(
    text: &str,
    effect: TemporalEffect,
) -> Result<Vec<TemporalRule>, String> {
    let text = strip_outer_parens(text.trim());
    if text.contains('"') || text.contains("||") {
        return Err(format!("unsupported opening_hours construct in '{text}'"));
    }
    let mut parsed = Vec::<ParsedRule>::new();
    let mut mentions_holidays = false;
    for (sequence, rule_text) in text.split(';').enumerate() {
        let rule_text = rule_text.trim();
        if rule_text.is_empty() {
            continue;
        }
        for (position, rule) in parse_rule_with_additions(rule_text)?
            .into_iter()
            .enumerate()
        {
            mentions_holidays |= rule.mentions_holidays;
            if rule.off {
                // `PH off` needs no rule: holiday-aware rules without the PH
                // bit are already inactive on listed holidays.
                continue;
            }
            if position == 0 {
                for earlier in parsed
                    .iter_mut()
                    .filter(|earlier| earlier.sequence < sequence)
                {
                    if earlier.dates.is_empty() && rule.dates.is_empty() {
                        earlier.day_mask &= !rule.day_mask;
                    } else if earlier.day_mask & rule.day_mask != 0 {
                        return Err(format!(
                            "overlapping dated rules in '{text}' are not supported"
                        ));
                    }
                }
            }
            parsed.push(ParsedRule { sequence, ..rule });
        }
    }
    let mut rules = Vec::new();
    for rule in parsed.into_iter().filter(|rule| rule.day_mask != 0) {
        let dates = if rule.dates.is_empty() {
            vec![None]
        } else {
            rule.dates.iter().copied().map(Some).collect()
        };
        for dates in dates {
            rules.push(TemporalRule {
                dates,
                weekdays_include_holidays: !mentions_holidays,
                ..TemporalRule::new(rule.day_mask, rule.intervals.clone(), effect)
            });
        }
    }
    if rules.is_empty() {
        return Err(format!("time domain '{text}' selects no time"));
    }
    Ok(rules)
}

pub(crate) fn strip_outer_parens(text: &str) -> &str {
    let mut text = text;
    while let Some(inner) = text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        if inner.contains('(') || inner.contains(')') {
            break;
        }
        text = inner.trim();
    }
    text
}

#[derive(Debug, Clone, Default)]
struct ParsedRule {
    sequence: usize,
    dates: Vec<DateRange>,
    day_mask: u8,
    intervals: Vec<MinuteInterval>,
    mentions_holidays: bool,
    off: bool,
}

/// One `;`-separated rule plus any `,`-joined additional rules
/// (`Mo-Fr 08:00-12:00, We 14:00-18:00`).
fn parse_rule_with_additions(text: &str) -> Result<Vec<ParsedRule>, String> {
    let mut rules = Vec::new();
    let mut cursor = RuleCursor::new(text);
    loop {
        rules.push(parse_rule(&mut cursor, text)?);
        cursor.skip_ws();
        if cursor.at_end() {
            return Ok(rules);
        }
        if !cursor.eat(',') {
            return Err(format!("unexpected text in time domain '{text}'"));
        }
    }
}

fn parse_rule(cursor: &mut RuleCursor<'_>, text: &str) -> Result<ParsedRule, String> {
    cursor.skip_ws();
    if cursor.eat_str("24/7") {
        return Ok(ParsedRule {
            day_mask: EVERY_DAY,
            ..ParsedRule::default()
        });
    }
    let dates = parse_date_selector(cursor, text)?;
    cursor.skip_ws();
    let (weekdays, mentions_holidays) = parse_weekday_selector(cursor, text)?;
    cursor.skip_ws();
    let intervals = parse_time_selector(cursor, text)?;
    cursor.skip_ws();
    let off = match cursor.peek_word().to_ascii_lowercase().as_str() {
        "off" | "closed" => {
            cursor.take_word();
            true
        }
        "open" => {
            cursor.take_word();
            false
        }
        _ => false,
    };
    if dates.is_empty() && weekdays.is_none() && intervals.is_empty() {
        return Err(format!("empty rule in time domain '{text}'"));
    }
    if off && (weekdays != Some(PUBLIC_HOLIDAY) || !intervals.is_empty() || !dates.is_empty()) {
        return Err(format!("only `PH off` is supported in '{text}'"));
    }
    Ok(ParsedRule {
        sequence: 0,
        dates,
        day_mask: weekdays.unwrap_or(EVERY_DAY),
        intervals,
        mentions_holidays,
        off,
    })
}

fn parse_date_selector(cursor: &mut RuleCursor<'_>, text: &str) -> Result<Vec<DateRange>, String> {
    let mut ranges = Vec::new();
    loop {
        let checkpoint = cursor.position;
        let Some(start) = parse_calendar_date(cursor, false) else {
            cursor.position = checkpoint;
            return Ok(ranges);
        };
        cursor.skip_ws();
        let end = if cursor.eat('-') {
            cursor.skip_ws();
            let end = parse_calendar_date(cursor, true)
                .ok_or_else(|| format!("invalid date range in '{text}'"))?;
            CalendarDate {
                year: end.date.year.or(start.date.year),
                month: if end.month_given {
                    end.date.month
                } else {
                    start.date.month
                },
                day: if end.day_given { end.date.day } else { 31 },
            }
        } else if start.day_given {
            start.date
        } else {
            CalendarDate {
                day: 31,
                ..start.date
            }
        };
        if start.date.year.is_some() != end.year.is_some() {
            return Err(format!(
                "date range mixes years and yearly dates in '{text}'"
            ));
        }
        ranges.push(DateRange {
            start: start.date,
            end,
        });
        let after_range = cursor.position;
        cursor.skip_ws();
        // A `,` continues the date list only when another date follows;
        // otherwise it starts an additional rule.
        if cursor.eat(',') {
            cursor.skip_ws();
            let next = cursor.position;
            if parse_calendar_date(cursor, false).is_some() {
                cursor.position = next;
                continue;
            }
        }
        cursor.position = after_range;
        return Ok(ranges);
    }
}

struct ParsedDate {
    date: CalendarDate,
    month_given: bool,
    day_given: bool,
}

/// `[YYYY] Mon [DD]`, the non-standard `DD Mon`, or (ending a range) a bare
/// day number.
fn parse_calendar_date(cursor: &mut RuleCursor<'_>, range_end: bool) -> Option<ParsedDate> {
    let checkpoint = cursor.position;
    let mut year = None;
    if let Some((value, 4)) = cursor.peek_number() {
        cursor.take_number();
        cursor.skip_ws();
        year = Some(value as i32);
    }
    if let Some(month) = month_number(&cursor.peek_word()) {
        cursor.take_word();
        let after_month = cursor.position;
        cursor.skip_ws();
        // A day follows unless the number starts a time (`Jan 08:00`).
        let day = match cursor.peek_number() {
            Some((day, digits))
                if digits <= 2 && !cursor.number_followed_by(':') && (1..=31).contains(&day) =>
            {
                cursor.take_number();
                Some(day as u8)
            }
            _ => {
                cursor.position = after_month;
                None
            }
        };
        return Some(ParsedDate {
            date: CalendarDate {
                year,
                month,
                day: day.unwrap_or(1),
            },
            month_given: true,
            day_given: day.is_some(),
        });
    }
    if year.is_some() {
        cursor.position = checkpoint;
        return None;
    }
    let (day, digits) = cursor.peek_number()?;
    if digits > 2 || !(1..=31).contains(&day) || cursor.number_followed_by(':') {
        return None;
    }
    cursor.take_number();
    let after_day = cursor.position;
    cursor.skip_ws();
    if let Some(month) = month_number(&cursor.peek_word()) {
        cursor.take_word();
        return Some(ParsedDate {
            date: CalendarDate {
                year: None,
                month,
                day: day as u8,
            },
            month_given: true,
            day_given: true,
        });
    }
    if range_end {
        cursor.position = after_day;
        return Some(ParsedDate {
            date: CalendarDate {
                year: None,
                month: 0,
                day: day as u8,
            },
            month_given: false,
            day_given: true,
        });
    }
    cursor.position = checkpoint;
    None
}

fn month_number(word: &str) -> Option<u8> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let word = word.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|month| *month == word)
        .map(|index| index as u8 + 1)
}

const WEEKDAYS: [(&str, u8); 7] = [
    ("mo", MONDAY),
    ("tu", TUESDAY),
    ("we", WEDNESDAY),
    ("th", THURSDAY),
    ("fr", FRIDAY),
    ("sa", SATURDAY),
    ("su", SUNDAY),
];

fn weekday_index(word: &str) -> Option<usize> {
    let word = word.to_ascii_lowercase();
    WEEKDAYS.iter().position(|(name, _)| *name == word)
}

/// The selected day mask (`None` without a weekday selector) and whether
/// `PH` was mentioned.
fn parse_weekday_selector(
    cursor: &mut RuleCursor<'_>,
    text: &str,
) -> Result<(Option<u8>, bool), String> {
    let mut mask = 0_u8;
    let mut mentions_holidays = false;
    let mut any = false;
    loop {
        let word = cursor.peek_word();
        let lower = word.to_ascii_lowercase();
        if lower == "ph" {
            cursor.take_word();
            mask |= PUBLIC_HOLIDAY;
            mentions_holidays = true;
        } else if lower == "sh" {
            return Err(format!("school holidays are not supported in '{text}'"));
        } else if let Some(start) = weekday_index(&word) {
            cursor.take_word();
            if cursor.peek() == Some('[') {
                return Err(format!(
                    "nth-weekday selectors are not supported in '{text}'"
                ));
            }
            let end = if cursor.eat('-') {
                weekday_index(&cursor.take_word())
                    .ok_or_else(|| format!("invalid weekday range in '{text}'"))?
            } else {
                start
            };
            let mut day = start;
            loop {
                mask |= WEEKDAYS[day].1;
                if day == end {
                    break;
                }
                day = (day + 1) % 7;
            }
        } else {
            break;
        }
        any = true;
        let after_item = cursor.position;
        cursor.skip_ws();
        if cursor.eat(',') {
            cursor.skip_ws();
            let next = cursor.peek_word().to_ascii_lowercase();
            if next == "ph" || next == "sh" || weekday_index(&next).is_some() {
                continue;
            }
        }
        cursor.position = after_item;
        break;
    }
    Ok((any.then_some(mask), mentions_holidays))
}

fn is_sun_event(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "sunrise" | "sunset" | "dawn" | "dusk"
    )
}

fn parse_time_selector(
    cursor: &mut RuleCursor<'_>,
    text: &str,
) -> Result<Vec<MinuteInterval>, String> {
    let mut intervals = Vec::new();
    loop {
        if is_sun_event(&cursor.peek_word()) {
            return Err(format!("sun-relative times are not supported in '{text}'"));
        }
        if cursor.peek_number().is_none() {
            return Ok(intervals);
        }
        if !cursor.number_followed_by(':') {
            return Err(format!("times must use HH:MM in '{text}'"));
        }
        let start = parse_clock(cursor, text)?;
        cursor.skip_ws();
        if !cursor.eat('-') {
            return Err(format!("open-ended times are not supported in '{text}'"));
        }
        cursor.skip_ws();
        if is_sun_event(&cursor.peek_word()) {
            return Err(format!("sun-relative times are not supported in '{text}'"));
        }
        let end = parse_clock(cursor, text)?;
        if start >= 1440 || end > 2 * 1440 {
            return Err(format!("time out of range in '{text}'"));
        }
        // Ends past midnight (`22:00-26:00`) wrap like `22:00-02:00`.
        let end = if end > 1440 { end - 1440 } else { end };
        if start == end {
            return Err(format!("empty time interval in '{text}'"));
        }
        intervals.push(MinuteInterval {
            start_minute: start,
            end_minute: end,
        });
        let after_interval = cursor.position;
        cursor.skip_ws();
        if cursor.eat(',') {
            cursor.skip_ws();
            if cursor.peek_number().is_some() {
                continue;
            }
        }
        cursor.position = after_interval;
        return Ok(intervals);
    }
}

fn parse_clock(cursor: &mut RuleCursor<'_>, text: &str) -> Result<u16, String> {
    let invalid = || format!("invalid time in '{text}'");
    let (hours, _) = cursor.take_number().ok_or_else(invalid)?;
    if !cursor.eat(':') {
        return Err(invalid());
    }
    let (minutes, digits) = cursor.take_number().ok_or_else(invalid)?;
    if digits != 2 || minutes > 59 || hours > 48 {
        return Err(invalid());
    }
    Ok((hours * 60 + minutes) as u16)
}

struct RuleCursor<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> RuleCursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.position..]
    }

    fn at_end(&self) -> bool {
        self.rest().trim().is_empty()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn skip_ws(&mut self) {
        let rest = self.rest();
        self.position += rest.len() - rest.trim_start().len();
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.position += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, expected: &str) -> bool {
        if self.rest().starts_with(expected) {
            self.position += expected.len();
            true
        } else {
            false
        }
    }

    fn peek_word(&self) -> String {
        self.rest()
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect()
    }

    fn take_word(&mut self) -> String {
        let word = self.peek_word();
        self.position += word.len();
        word
    }

    /// Leading decimal number and its digit count.
    fn peek_number(&self) -> Option<(u32, usize)> {
        let digits = self.rest().chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 4 {
            return None;
        }
        Some((self.rest()[..digits].parse().ok()?, digits))
    }

    fn take_number(&mut self) -> Option<(u32, usize)> {
        let number = self.peek_number()?;
        self.position += number.1;
        Some(number)
    }

    fn number_followed_by(&self, character: char) -> bool {
        self.rest()
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with(character)
    }
}

#[cfg(test)]
mod tests;
