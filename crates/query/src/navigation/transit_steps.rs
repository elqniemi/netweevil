//! Localized journey steps for public transport directions, from the same
//! phrase tables as street instructions.

use super::phrases::{Locale, locales, resolve_locale};

/// How a street leg of a journey is travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreetLegMode {
    Walk,
    Bicycle,
    Car,
}

/// One step of a public transport journey.
#[derive(Debug, Clone, Copy)]
pub enum TransitStep<'a> {
    Wait {
        stop: &'a str,
        line: Option<&'a str>,
    },
    Board {
        line: &'a str,
        stop: &'a str,
        headsign: Option<&'a str>,
    },
    Ride {
        line: &'a str,
        from: &'a str,
        to: &'a str,
    },
    Alight {
        stop: &'a str,
    },
    Street {
        mode: StreetLegMode,
        from: &'a str,
        to: &'a str,
    },
}

/// The phrase table of one language.
#[derive(Clone, Copy)]
pub struct Phrasebook {
    locale: &'static Locale,
}

impl Phrasebook {
    /// The table for a BCP 47 tag, or English plus a warning when no table
    /// matches.
    pub fn for_language(language: Option<&str>) -> (Self, Option<String>) {
        let requested = language.unwrap_or("en");
        match resolve_locale(requested) {
            Some(locale) => (Self { locale }, None),
            None => (
                Self {
                    locale: &locales()[0],
                },
                Some(format!(
                    "no phrase table for language '{requested}'; using English"
                )),
            ),
        }
    }

    pub fn language(&self) -> &str {
        &self.locale.locale
    }

    pub(crate) fn locale(&self) -> &'static Locale {
        self.locale
    }

    pub fn transit_step(&self, step: TransitStep<'_>) -> String {
        let locale = self.locale;
        let fragment = match step {
            TransitStep::Wait { stop, line: None } => {
                locale.fragment("transit_wait", &[("stop", stop)])
            }
            TransitStep::Wait {
                stop,
                line: Some(line),
            } => locale.fragment("transit_wait_line", &[("line", line), ("stop", stop)]),
            TransitStep::Board {
                line,
                stop,
                headsign: Some(headsign),
            } => locale.fragment(
                "transit_board_toward",
                &[("headsign", headsign), ("line", line), ("stop", stop)],
            ),
            TransitStep::Board {
                line,
                stop,
                headsign: None,
            } => locale.fragment("transit_board", &[("line", line), ("stop", stop)]),
            TransitStep::Ride { line, from, to } => locale.fragment(
                "transit_ride",
                &[("from", from), ("line", line), ("to", to)],
            ),
            TransitStep::Alight { stop } => locale.fragment("transit_alight", &[("stop", stop)]),
            TransitStep::Street { mode, from, to } => {
                let key = match mode {
                    StreetLegMode::Walk => "street_leg_walk",
                    StreetLegMode::Bicycle => "street_leg_bicycle",
                    StreetLegMode::Car => "street_leg_car",
                };
                locale.fragment(key, &[("from", from), ("to", to)])
            }
        };
        locale.sentence(&fragment)
    }
}

#[cfg(test)]
mod tests;
