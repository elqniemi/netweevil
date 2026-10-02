use super::parse_osm_time_domain;
use netweevil_core::{
    CalendarDate, DateRange, EVERY_DAY, FRIDAY, MONDAY, MinuteInterval, PUBLIC_HOLIDAY, SATURDAY,
    SUNDAY, THURSDAY, TUESDAY, TemporalEffect, WEDNESDAY,
};

const WEEKDAYS: u8 = MONDAY | TUESDAY | WEDNESDAY | THURSDAY | FRIDAY;

fn minutes(start: &str, end: &str) -> MinuteInterval {
    let parse = |value: &str| {
        let (hours, minutes) = value.split_once(':').unwrap();
        hours.parse::<u16>().unwrap() * 60 + minutes.parse::<u16>().unwrap()
    };
    MinuteInterval {
        start_minute: parse(start),
        end_minute: parse(end),
    }
}

fn date(year: Option<i32>, month: u8, day: u8) -> CalendarDate {
    CalendarDate { year, month, day }
}

/// `(day_mask, intervals, dates)` of each parsed rule.
type RuleShape = (u8, Vec<MinuteInterval>, Option<DateRange>);

fn shapes(text: &str) -> Vec<RuleShape> {
    parse_osm_time_domain(text, TemporalEffect::Closed)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
        .into_iter()
        .map(|rule| (rule.day_mask, rule.intervals, rule.dates))
        .collect()
}

#[test]
fn parses_weekday_and_time_domains_from_real_conditionals() {
    let cases: Vec<(&str, Vec<RuleShape>)> = vec![
        (
            "Mo-Fr 07:00-12:00",
            vec![(WEEKDAYS, vec![minutes("07:00", "12:00")], None)],
        ),
        (
            "(05:00-12:00)",
            vec![(EVERY_DAY, vec![minutes("05:00", "12:00")], None)],
        ),
        (
            "07:00-12:00, 18:00-21:00",
            vec![(
                EVERY_DAY,
                vec![minutes("07:00", "12:00"), minutes("18:00", "21:00")],
                None,
            )],
        ),
        (
            "Mo-Th 09:00-18:00; Fr 09:00-21:00; Sa 08:30-17:30",
            vec![
                (
                    MONDAY | TUESDAY | WEDNESDAY | THURSDAY,
                    vec![minutes("09:00", "18:00")],
                    None,
                ),
                (FRIDAY, vec![minutes("09:00", "21:00")], None),
                (SATURDAY, vec![minutes("08:30", "17:30")], None),
            ],
        ),
        (
            "Fr 23:00-24:00; Sa-Su; Mo 00:00-07:30",
            vec![
                (FRIDAY, vec![minutes("23:00", "24:00")], None),
                (SATURDAY | SUNDAY, vec![], None),
                (MONDAY, vec![minutes("00:00", "07:30")], None),
            ],
        ),
        (
            "Su-Th 18:00-11:00",
            vec![(
                SUNDAY | MONDAY | TUESDAY | WEDNESDAY | THURSDAY,
                vec![minutes("18:00", "11:00")],
                None,
            )],
        ),
        (
            "Tu,Fr,Sa 06:00-19:00",
            vec![(
                TUESDAY | FRIDAY | SATURDAY,
                vec![minutes("06:00", "19:00")],
                None,
            )],
        ),
        (
            "Su-Fr",
            vec![(EVERY_DAY & !SATURDAY & !PUBLIC_HOLIDAY, vec![], None)],
        ),
        (
            "22:00-26:00",
            vec![(EVERY_DAY, vec![minutes("22:00", "02:00")], None)],
        ),
        ("24/7", vec![(EVERY_DAY, vec![], None)]),
    ];
    for (text, expected) in cases {
        assert_eq!(shapes(text), expected, "{text}");
    }
}

#[test]
fn later_rules_replace_earlier_days_and_additional_rules_add() {
    assert_eq!(
        shapes("Mo-Fr 08:00-18:00; We 10:00-12:00"),
        vec![
            (
                MONDAY | TUESDAY | THURSDAY | FRIDAY,
                vec![minutes("08:00", "18:00")],
                None
            ),
            (WEDNESDAY, vec![minutes("10:00", "12:00")], None),
        ]
    );
    assert_eq!(
        shapes("Mo-Fr 08:00-12:00, We 14:00-18:00"),
        vec![
            (WEEKDAYS, vec![minutes("08:00", "12:00")], None),
            (WEDNESDAY, vec![minutes("14:00", "18:00")], None),
        ]
    );
}

#[test]
fn parses_yearly_and_absolute_date_windows() {
    let yearly = |start: CalendarDate, end: CalendarDate| {
        vec![(EVERY_DAY, vec![], Some(DateRange { start, end }))]
    };
    let cases = vec![
        ("Nov 1-Apr 1", yearly(date(None, 11, 1), date(None, 4, 1))),
        ("1 Nov - 1 Apr", yearly(date(None, 11, 1), date(None, 4, 1))),
        ("Mar 01-Jul 01", yearly(date(None, 3, 1), date(None, 7, 1))),
        ("Jan-Mar", yearly(date(None, 1, 1), date(None, 3, 31))),
        ("Dec 24-26", yearly(date(None, 12, 24), date(None, 12, 26))),
        (
            "2025 Jan 13-2025 Apr 4",
            yearly(date(Some(2025), 1, 13), date(Some(2025), 4, 4)),
        ),
        (
            "(2024 May 06 - 2024 Jul 26)",
            yearly(date(Some(2024), 5, 6), date(Some(2024), 7, 26)),
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(shapes(text), expected, "{text}");
    }
    assert_eq!(
        shapes("Nov-Mar Mo-Fr 07:00-09:00"),
        vec![(
            WEEKDAYS,
            vec![minutes("07:00", "09:00")],
            Some(DateRange {
                start: date(None, 11, 1),
                end: date(None, 3, 31),
            })
        )]
    );
}

#[test]
fn holiday_mentions_switch_rules_to_the_holiday_calendar() {
    let rules =
        parse_osm_time_domain("Mo-Fr 07:00-09:00; PH off", TemporalEffect::Closed).expect("parses");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].day_mask, WEEKDAYS);
    assert!(!rules[0].weekdays_include_holidays);
    let rules =
        parse_osm_time_domain("Mo-Fr 07:00-09:00", TemporalEffect::OpenOnly).expect("parses");
    assert!(rules[0].weekdays_include_holidays);
    assert_eq!(rules[0].effect, TemporalEffect::OpenOnly);
    let rules = parse_osm_time_domain("Sa,PH 10:00-12:00", TemporalEffect::Closed).expect("parses");
    assert_eq!(rules[0].day_mask, SATURDAY | PUBLIC_HOLIDAY);
}

#[test]
fn rejects_constructs_outside_the_supported_subset() {
    for text in [
        "sunset-sunrise",
        "sunrise - sunset",
        "08:00-sunset",
        "Mo,Tu,Th,Fr 08:05-08:35, 14:45-15:15; We 08:05-08:35, 12:15-12:45; SH off, PH off",
        "5-11,18-20",
        "Mo[1] 08:00-10:00",
        "Mo-Fr 08:00+",
        "Mo-Fr 08:00-10:00 \"school\"",
        "Mo off",
        "destination",
        "wet",
        "",
    ] {
        assert!(
            parse_osm_time_domain(text, TemporalEffect::Closed).is_err(),
            "{text:?} should be rejected"
        );
    }
}
