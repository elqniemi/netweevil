use std::collections::BTreeSet;

use super::*;

fn locale(tag: &str) -> &'static Locale {
    resolve_locale(tag).unwrap_or_else(|| panic!("no table for {tag}"))
}

#[test]
fn every_table_defines_every_phrase_with_exactly_its_placeholders() {
    let expected_keys = PHRASE_KEYS
        .iter()
        .map(|(key, _)| *key)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_keys.len(),
        PHRASE_KEYS.len(),
        "duplicate phrase key"
    );
    for locale in locales() {
        let keys = locale
            .phrases
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(keys, expected_keys, "{} phrase keys", locale.locale);
        for (key, expected) in PHRASE_KEYS {
            let template = &locale.phrases[*key];
            let mut used = placeholders(template);
            used.sort_unstable();
            assert_eq!(&used, expected, "{} {key}: {template}", locale.locale);
        }
        for (table, keys) in [
            (&locale.directions, DIRECTION_KEYS),
            (&locale.sides, SIDE_KEYS),
            (&locale.cardinals, CARDINAL_KEYS),
        ] {
            assert_eq!(
                table.keys().map(String::as_str).collect::<Vec<_>>(),
                keys.iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>(),
                "{} word table",
                locale.locale
            );
            assert!(table.values().all(|word| !word.trim().is_empty()));
        }
        for template in locale.unit_and_ordinal_templates() {
            assert_eq!(
                placeholders(template),
                ["n"],
                "{}: {template}",
                locale.locale
            );
        }
        assert_eq!(placeholders(&locale.sentence), ["text"]);
    }
}

#[test]
fn ships_the_documented_languages() {
    let tags = guidance_languages()
        .into_iter()
        .map(|(tag, _)| tag)
        .collect::<Vec<_>>();
    assert_eq!(tags, ["en", "nl", "de", "fr", "fi", "zh-Hant"]);
    assert!(locale("de").has_units_in() && locale("fi").has_units_in());
}

#[test]
fn language_tags_resolve_through_aliases_and_subtag_truncation() {
    for (requested, expected) in [
        ("en", Some("en")),
        ("en-AU", Some("en")),
        ("nl-BE", Some("nl")),
        ("NL", Some("nl")),
        ("de_CH", Some("de")),
        ("fr-CA", Some("fr")),
        ("fi-FI", Some("fi")),
        ("zh-HK", Some("zh-Hant")),
        ("zh-Hant-HK", Some("zh-Hant")),
        ("zh", Some("zh-Hant")),
        ("sv-SE", None),
        ("", None),
        ("-", None),
    ] {
        assert_eq!(
            resolve_locale(requested).map(|locale| locale.locale.as_str()),
            expected,
            "{requested:?}"
        );
    }
}

#[test]
fn distances_round_like_speech_and_follow_plural_rules() {
    use DistanceUnits::{Imperial, Metric};
    for (tag, meters, units, after_in, expected) in [
        ("en", 1.0, Metric, false, "1 meter"),
        ("en", 7.4, Metric, false, "7 meters"),
        ("en", 44.0, Metric, false, "40 meters"),
        ("en", 476.0, Metric, false, "500 meters"),
        ("en", 980.0, Metric, false, "1 kilometer"),
        ("en", 1_040.0, Metric, false, "1 kilometer"),
        ("en", 1_550.0, Metric, false, "1.6 kilometers"),
        ("en", 12_345.0, Metric, false, "12 kilometers"),
        ("en", 100.0, Imperial, false, "350 feet"),
        ("en", 300.0, Imperial, false, "0.2 miles"),
        ("en", 1_609.344, Imperial, false, "1 mile"),
        ("en", 25_000.0, Imperial, false, "16 miles"),
        ("fr", 1_500.0, Metric, false, "1,5 kilomètre"),
        ("fr", 2_400.0, Metric, false, "2,4 kilomètres"),
        ("fr", 1.0, Metric, false, "1 mètre"),
        ("de", 500.0, Metric, true, "500 Metern"),
        ("de", 500.0, Metric, false, "500 Meter"),
        ("de", 1_000.0, Metric, true, "1 Kilometer"),
        ("de", 2_500.0, Metric, true, "2,5 Kilometern"),
        ("nl", 2_500.0, Metric, false, "2,5 kilometer"),
        ("fi", 500.0, Metric, false, "500 metriä"),
        ("fi", 1_000.0, Metric, false, "1 kilometri"),
        ("fi", 1_000.0, Metric, true, "1 kilometrin"),
        ("fi", 2_500.0, Metric, false, "2,5 kilometriä"),
        ("zh-Hant", 1_500.0, Metric, false, "1.5公里"),
        ("zh-Hant", 300.0, Metric, true, "300米"),
    ] {
        assert_eq!(
            locale(tag).distance(meters, units, after_in),
            expected,
            "{tag} {meters} m {units:?} after_in={after_in}"
        );
    }
}

#[test]
fn ordinals_follow_each_language_rule() {
    for (tag, n, expected) in [
        ("en", 1, "1st"),
        ("en", 2, "2nd"),
        ("en", 3, "3rd"),
        ("en", 4, "4th"),
        ("en", 11, "11th"),
        ("en", 12, "12th"),
        ("en", 13, "13th"),
        ("en", 21, "21st"),
        ("en", 22, "22nd"),
        ("en", 101, "101st"),
        ("en", 111, "111th"),
        ("fr", 1, "1re"),
        ("fr", 2, "2e"),
        ("nl", 1, "1e"),
        ("de", 2, "2."),
        ("fi", 3, "3."),
        ("zh-Hant", 3, "第3個"),
    ] {
        assert_eq!(locale(tag).ordinal(n), expected, "{tag} {n}");
    }
}

#[test]
fn placeholders_fill_in_one_pass_and_sentences_capitalize() {
    assert_eq!(
        fill(
            "turn {direction} onto {street}",
            &[("street", "{direction} Road"), ("direction", "left")]
        ),
        "turn left onto {direction} Road"
    );
    assert_eq!(fill("keep {unknown}", &[]), "keep {unknown}");
    assert_eq!(fill("open {brace", &[]), "open {brace");
    assert_eq!(locale("fr").sentence("à gauche"), "À gauche.");
    assert_eq!(locale("zh-Hant").sentence("左轉"), "左轉。");
    assert_eq!(locale("en").sentence(""), ".");
}
