//! Data-driven phrase tables for guidance text.
//!
//! Each language is one YAML file under `locales/`, embedded at compile
//! time. A table holds sentence fragments with `{placeholder}`s, word lists
//! for directions, sides and compass points, unit forms per CLDR plural
//! category, and ordinal forms per CLDR ordinal category. The renderer fills
//! placeholders in one pass (inserted street names are never re-scanned),
//! capitalizes the first letter and wraps the fragment in the table's
//! `sentence` pattern. [`PHRASE_KEYS`] is the contract every table meets;
//! the tests check it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::DistanceUnits;

/// Embedded tables. English comes first and is the fallback language.
const LOCALE_SOURCES: &[(&str, &str)] = &[
    ("en", include_str!("locales/en.yaml")),
    ("nl", include_str!("locales/nl.yaml")),
    ("de", include_str!("locales/de.yaml")),
    ("fr", include_str!("locales/fr.yaml")),
    ("fi", include_str!("locales/fi.yaml")),
    ("zh-Hant", include_str!("locales/zh-Hant.yaml")),
];

/// Every phrase key with the placeholders its template must use. The
/// tests hold every table to this list.
#[cfg(test)]
pub(crate) const PHRASE_KEYS: &[(&str, &[&str])] = &[
    ("depart", &["cardinal"]),
    ("depart_street", &["cardinal", "street"]),
    ("continue", &[]),
    ("continue_street", &["street"]),
    ("turn", &["direction"]),
    ("turn_street", &["direction", "street"]),
    ("turn_toward", &["direction", "toward"]),
    ("uturn", &[]),
    ("uturn_street", &["street"]),
    ("ramp", &["side"]),
    ("ramp_toward", &["side", "toward"]),
    ("ramp_straight", &[]),
    ("ramp_straight_toward", &["toward"]),
    ("exit", &["side"]),
    ("exit_toward", &["side", "toward"]),
    ("exit_number", &["exit", "side"]),
    ("exit_number_toward", &["exit", "side", "toward"]),
    ("keep", &["side"]),
    ("keep_street", &["side", "street"]),
    ("keep_toward", &["side", "toward"]),
    ("keep_straight", &[]),
    ("keep_straight_street", &["street"]),
    ("keep_straight_toward", &["toward"]),
    ("merge", &[]),
    ("merge_street", &["street"]),
    ("roundabout_enter", &[]),
    ("roundabout_enter_exit", &["ordinal"]),
    ("roundabout_enter_exit_street", &["ordinal", "street"]),
    ("roundabout_exit", &[]),
    ("roundabout_exit_street", &["street"]),
    ("ferry_enter", &[]),
    ("ferry_enter_street", &["street"]),
    ("ferry_exit", &[]),
    ("stairs", &[]),
    ("stairs_up", &[]),
    ("stairs_down", &[]),
    ("escalator", &[]),
    ("escalator_up", &[]),
    ("escalator_down", &[]),
    ("elevator", &[]),
    ("elevator_up", &[]),
    ("elevator_down", &[]),
    ("travelator", &[]),
    ("arrive", &[]),
    ("arrive_side", &["side"]),
    ("arrive_pre_alert", &["distance"]),
    ("verbal_pre_alert", &["distance", "instruction"]),
    ("verbal_then", &["instruction", "next"]),
    ("verbal_post_transition", &["distance"]),
    ("transit_wait", &["stop"]),
    ("transit_wait_line", &["line", "stop"]),
    ("transit_board", &["line", "stop"]),
    ("transit_board_toward", &["headsign", "line", "stop"]),
    ("transit_ride", &["from", "line", "to"]),
    ("transit_alight", &["stop"]),
    ("street_leg_walk", &["from", "to"]),
    ("street_leg_bicycle", &["from", "to"]),
    ("street_leg_car", &["from", "to"]),
];

#[cfg(test)]
pub(crate) const DIRECTION_KEYS: &[&str] = &[
    "slight_left",
    "left",
    "sharp_left",
    "slight_right",
    "right",
    "sharp_right",
];
#[cfg(test)]
pub(crate) const SIDE_KEYS: &[&str] = &["left", "right"];
pub(crate) const CARDINAL_KEYS: &[&str] = &[
    "north",
    "northeast",
    "east",
    "southeast",
    "south",
    "southwest",
    "west",
    "northwest",
];

/// CLDR cardinal plural rule of a language, as far as distances need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PluralRule {
    /// `one` for exactly 1 without visible decimals (en, nl, de, fi).
    OneExactly,
    /// `one` when the integer part is 0 or 1 (fr: "1,5 kilomètre").
    ZeroOrOne,
    /// Only `other` (zh).
    None,
}

/// CLDR ordinal rule of a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrdinalRule {
    /// one/two/few/other: 1st, 2nd, 3rd, 4th, 11th, 21st.
    English,
    /// `one` for 1 only (fr: 1re, 2e).
    OneExactly,
    /// Only `other` (nl 2e, de 2., fi 2., zh 第2個).
    OtherOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluralCategory {
    One,
    Two,
    Few,
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluralForms {
    #[serde(default)]
    one: Option<String>,
    #[serde(default)]
    two: Option<String>,
    #[serde(default)]
    few: Option<String>,
    other: String,
}

impl PluralForms {
    fn pick(&self, category: PluralCategory) -> &str {
        let form = match category {
            PluralCategory::One => self.one.as_deref(),
            PluralCategory::Two => self.two.as_deref(),
            PluralCategory::Few => self.few.as_deref(),
            PluralCategory::Other => None,
        };
        form.unwrap_or(&self.other)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitForms {
    meters: PluralForms,
    kilometers: PluralForms,
    feet: PluralForms,
    miles: PluralForms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DistanceUnit {
    Meters,
    Kilometers,
    Feet,
    Miles,
}

impl UnitForms {
    fn forms(&self, unit: DistanceUnit) -> &PluralForms {
        match unit {
            DistanceUnit::Meters => &self.meters,
            DistanceUnit::Kilometers => &self.kilometers,
            DistanceUnit::Feet => &self.feet,
            DistanceUnit::Miles => &self.miles,
        }
    }
}

/// One language's phrase table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Locale {
    pub(crate) locale: String,
    pub(crate) name: String,
    #[serde(default)]
    aliases: Vec<String>,
    plural: PluralRule,
    ordinal: OrdinalRule,
    decimal_separator: String,
    list_separator: String,
    sentence: String,
    units: UnitForms,
    /// Unit forms after "in" when the language inflects them there.
    #[serde(default)]
    units_in: Option<UnitForms>,
    ordinals: PluralForms,
    pub(crate) directions: BTreeMap<String, String>,
    pub(crate) sides: BTreeMap<String, String>,
    pub(crate) cardinals: BTreeMap<String, String>,
    pub(crate) phrases: BTreeMap<String, String>,
}

pub(crate) fn locales() -> &'static [Locale] {
    static LOCALES: OnceLock<Vec<Locale>> = OnceLock::new();
    LOCALES.get_or_init(|| {
        LOCALE_SOURCES
            .iter()
            .map(|(tag, source)| {
                let locale: Locale = serde_yaml::from_str(source)
                    .unwrap_or_else(|error| panic!("phrase table {tag} is invalid: {error}"));
                assert_eq!(locale.locale, *tag, "phrase table tag mismatch");
                locale
            })
            .collect()
    })
}

fn english() -> &'static Locale {
    &locales()[0]
}

/// Languages with a phrase table, as `(tag, native name)`.
pub fn guidance_languages() -> Vec<(&'static str, &'static str)> {
    locales()
        .iter()
        .map(|locale| (locale.locale.as_str(), locale.name.as_str()))
        .collect()
}

/// Resolves a BCP 47 tag to a table: exact tag or alias first, then the
/// tag with trailing subtags removed (`nl-BE` → `nl`). Returns `None` when
/// nothing matches; callers fall back to English.
pub(crate) fn resolve_locale(requested: &str) -> Option<&'static Locale> {
    let normalized = requested.trim().replace('_', "-").to_ascii_lowercase();
    let mut candidate = normalized.as_str();
    loop {
        if candidate.is_empty() {
            return None;
        }
        let found = locales().iter().find(|locale| {
            locale.locale.eq_ignore_ascii_case(candidate)
                || locale
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(candidate))
        });
        if found.is_some() {
            return found;
        }
        candidate = candidate.rsplit_once('-')?.0;
    }
}

/// A distance rounded the way it is spoken, before localization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpokenDistance {
    pub(crate) value: f64,
    pub(crate) decimals: usize,
    pub(crate) unit: DistanceUnit,
}

const FEET_PER_METER: f64 = 3.280_84;
const METERS_PER_MILE: f64 = 1_609.344;

fn round_to(value: f64, step: f64) -> f64 {
    (value / step).round() * step
}

/// Metric: whole metres below 10 m, tens below 100 m, fifties below 1 km,
/// then kilometres with one decimal below 10 km. Imperial: feet below a
/// tenth of a mile (tens below 100 ft, then fifties), then miles likewise.
pub(crate) fn spoken_distance(meters: f64, units: DistanceUnits) -> SpokenDistance {
    let meters = meters.max(0.0);
    let decimal_or_whole = |value: f64, unit| {
        if value < 9.95 {
            let value = round_to(value, 0.1);
            let decimals = usize::from(value.fract().abs() > 1e-9);
            SpokenDistance {
                value,
                decimals,
                unit,
            }
        } else {
            SpokenDistance {
                value: value.round(),
                decimals: 0,
                unit,
            }
        }
    };
    let small = |value: f64, unit| {
        let value = if value < 9.5 {
            value.round().max(1.0)
        } else if value < 95.0 {
            round_to(value, 10.0)
        } else {
            round_to(value, 50.0)
        };
        SpokenDistance {
            value,
            decimals: 0,
            unit,
        }
    };
    match units {
        DistanceUnits::Metric => {
            let spoken = small(meters, DistanceUnit::Meters);
            if spoken.value < 1_000.0 {
                spoken
            } else {
                decimal_or_whole(meters / 1_000.0, DistanceUnit::Kilometers)
            }
        }
        DistanceUnits::Imperial => {
            let miles = meters / METERS_PER_MILE;
            if miles < 0.095 {
                small(meters * FEET_PER_METER, DistanceUnit::Feet)
            } else {
                decimal_or_whole(miles, DistanceUnit::Miles)
            }
        }
    }
}

impl Locale {
    /// The template for `key`, falling back to English when the table lacks
    /// it (the tests make sure none do).
    fn template<'a>(&'a self, key: &'a str) -> &'a str {
        self.phrases
            .get(key)
            .or_else(|| english().phrases.get(key))
            .map_or(key, String::as_str)
    }

    /// Fills `key`'s placeholders. Unknown placeholders stay verbatim.
    pub(crate) fn fragment(&self, key: &str, args: &[(&str, &str)]) -> String {
        fill(self.template(key), args)
    }

    /// Capitalizes a fragment and wraps it in the sentence pattern.
    pub(crate) fn sentence(&self, fragment: &str) -> String {
        let mut chars = fragment.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
            None => String::new(),
        };
        fill(&self.sentence, &[("text", &capitalized)])
    }

    pub(crate) fn word<'a>(table: &'a BTreeMap<String, String>, key: &'a str) -> &'a str {
        table.get(key).map_or(key, String::as_str)
    }

    pub(crate) fn direction<'a>(&'a self, key: &'a str) -> &'a str {
        Self::word(&self.directions, key)
    }

    pub(crate) fn side<'a>(&'a self, key: &'a str) -> &'a str {
        Self::word(&self.sides, key)
    }

    pub(crate) fn cardinal<'a>(&'a self, key: &'a str) -> &'a str {
        Self::word(&self.cardinals, key)
    }

    pub(crate) fn list(&self, items: &[String]) -> String {
        items.join(&self.list_separator)
    }

    pub(crate) fn plural_category(&self, number: f64, decimals: usize) -> PluralCategory {
        let whole = decimals == 0 && number.fract() == 0.0;
        match self.plural {
            PluralRule::OneExactly if whole && number == 1.0 => PluralCategory::One,
            PluralRule::ZeroOrOne if number.trunc() == 0.0 || number.trunc() == 1.0 => {
                PluralCategory::One
            }
            _ => PluralCategory::Other,
        }
    }

    pub(crate) fn ordinal_category(&self, n: u32) -> PluralCategory {
        match self.ordinal {
            OrdinalRule::English => match (n % 10, n % 100) {
                (1, tens) if tens != 11 => PluralCategory::One,
                (2, tens) if tens != 12 => PluralCategory::Two,
                (3, tens) if tens != 13 => PluralCategory::Few,
                _ => PluralCategory::Other,
            },
            OrdinalRule::OneExactly if n == 1 => PluralCategory::One,
            _ => PluralCategory::Other,
        }
    }

    pub(crate) fn ordinal(&self, n: u32) -> String {
        let form = self.ordinals.pick(self.ordinal_category(n));
        fill(form, &[("n", &n.to_string())])
    }

    pub(crate) fn number(&self, value: f64, decimals: usize) -> String {
        let text = format!("{value:.decimals$}");
        if decimals == 0 {
            text
        } else {
            text.replace('.', &self.decimal_separator)
        }
    }

    /// A localized distance. `after_in` selects the forms used inside
    /// "in {distance}" phrases.
    pub(crate) fn distance(&self, meters: f64, units: DistanceUnits, after_in: bool) -> String {
        let spoken = spoken_distance(meters, units);
        let table = if after_in {
            self.units_in.as_ref().unwrap_or(&self.units)
        } else {
            &self.units
        };
        let category = self.plural_category(spoken.value, spoken.decimals);
        let form = table.forms(spoken.unit).pick(category);
        fill(form, &[("n", &self.number(spoken.value, spoken.decimals))])
    }

    #[cfg(test)]
    pub(crate) fn has_units_in(&self) -> bool {
        self.units_in.is_some()
    }

    #[cfg(test)]
    pub(crate) fn unit_and_ordinal_templates(&self) -> Vec<&str> {
        let mut forms = vec![&self.ordinals];
        for table in std::iter::once(&self.units).chain(self.units_in.as_ref()) {
            forms.extend([&table.meters, &table.kilometers, &table.feet, &table.miles]);
        }
        forms
            .into_iter()
            .flat_map(|forms| {
                [&forms.one, &forms.two, &forms.few]
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .chain([forms.other.as_str()])
            })
            .collect()
    }
}

/// Replaces `{name}` placeholders in one left-to-right pass.
pub(crate) fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Placeholder names used by a template, in order of appearance.
#[cfg(test)]
pub(crate) fn placeholders(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        names.push(&after[..close]);
        rest = &after[close + 1..];
    }
    names
}

#[cfg(test)]
mod tests;
