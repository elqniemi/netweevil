//! OSM physical vehicle restrictions: `maxheight`, `maxwidth`, `maxlength`,
//! `maxweight`, `maxaxleload`, `hazmat*`, `trailer`, `hgv_articulated`, and
//! the unit syntax they share.

use netweevil_core::{
    AccessMask, AdrTunnelCategory, VehicleRestriction, VehicleRestrictionKind as Kind,
};
use osmpbfreader::Tags;

use crate::classify::tag;

const METRES_PER_FOOT: f64 = 0.3048;
const METRES_PER_INCH: f64 = 0.0254;
const TONNES_PER_POUND: f64 = 0.000_453_592_37;
const TONNES_PER_SHORT_TON: f64 = 0.907_184_74;
const TONNES_PER_LONG_TON: f64 = 1.016_046_908_8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    Height,
    Width,
    Length,
    Weight,
    AxleLoad,
}

/// Restriction keys and the mode suffixes that scope them. A plain key
/// binds every vehicle; `maxweight:hgv` binds only heavy goods vehicles.
const DIMENSION_KEYS: [(&str, Dimension); 7] = [
    ("maxheight", Dimension::Height),
    ("maxheight:physical", Dimension::Height),
    ("maxwidth", Dimension::Width),
    ("maxwidth:physical", Dimension::Width),
    ("maxlength", Dimension::Length),
    ("maxweight", Dimension::Weight),
    ("maxaxleload", Dimension::AxleLoad),
];

const MODE_SUFFIXES: [(&str, u16); 6] = [
    ("", AccessMask::VEHICLES),
    (":hgv", AccessMask::HGV),
    (":bus", AccessMask::BUS),
    (":psv", AccessMask::BUS | AccessMask::TAXI),
    (":motorcar", AccessMask::CAR),
    (":motor_vehicle", AccessMask::MOTOR_VEHICLES),
];

/// Physical restrictions of a way, deduplicated.
pub(crate) fn way_vehicle_restrictions(tags: &Tags) -> Vec<VehicleRestriction> {
    let mut restrictions = Vec::new();
    for (key, dimension) in DIMENSION_KEYS {
        for (suffix, modes) in MODE_SUFFIXES {
            // `maxheight:physical:hgv` is not a tagging scheme.
            if key.contains(':') && !suffix.is_empty() {
                continue;
            }
            let Some(kind) = tag(tags, &format!("{key}{suffix}"))
                .and_then(|value| dimension_limit(dimension, value))
            else {
                continue;
            };
            push_unique(&mut restrictions, modes, kind);
        }
    }
    if tag(tags, "hazmat") == Some("no") {
        push_unique(&mut restrictions, AccessMask::VEHICLES, Kind::NoHazmat);
    }
    if tag(tags, "hazmat:water") == Some("no") {
        push_unique(&mut restrictions, AccessMask::VEHICLES, Kind::NoHazmatWater);
    }
    if let Some(category) = tag(tags, "hazmat:adr_tunnel_cat").and_then(AdrTunnelCategory::parse) {
        push_unique(
            &mut restrictions,
            AccessMask::VEHICLES,
            Kind::AdrTunnel(category),
        );
    }
    if tag(tags, "trailer") == Some("no") {
        push_unique(&mut restrictions, AccessMask::VEHICLES, Kind::NoTrailer);
    }
    if tag(tags, "hgv_articulated") == Some("no") {
        push_unique(&mut restrictions, AccessMask::HGV, Kind::NoArticulated);
    }
    restrictions
}

fn push_unique(restrictions: &mut Vec<VehicleRestriction>, modes: u16, kind: Kind) {
    let restriction = VehicleRestriction {
        modes: AccessMask::new(modes),
        kind,
    };
    if !restrictions.contains(&restriction) {
        restrictions.push(restriction);
    }
}

fn dimension_limit(dimension: Dimension, value: &str) -> Option<Kind> {
    let centimetres = |metres: f64| (metres * 100.0).round() as u32;
    let kilograms = |tonnes: f64| (tonnes * 1000.0).round() as u32;
    Some(match dimension {
        Dimension::Height => Kind::MaxHeightCm(centimetres(parse_length_m(value)?)),
        Dimension::Width => Kind::MaxWidthCm(centimetres(parse_length_m(value)?)),
        Dimension::Length => Kind::MaxLengthCm(centimetres(parse_length_m(value)?)),
        Dimension::Weight => Kind::MaxWeightKg(kilograms(parse_weight_t(value)?)),
        Dimension::AxleLoad => Kind::MaxAxleLoadKg(kilograms(parse_weight_t(value)?)),
    })
}

/// A restriction derived from a vehicle-property condition such as
/// `hgv:conditional=no @ (weight>7.5)`: vehicles above `value` are denied.
/// `property` is the condition's left-hand side.
pub(crate) fn conditional_limit(property: &str, value: &str) -> Option<Kind> {
    let dimension = match property {
        "height" => Dimension::Height,
        "width" => Dimension::Width,
        "length" => Dimension::Length,
        "weight" => Dimension::Weight,
        "axleload" => Dimension::AxleLoad,
        _ => return None,
    };
    dimension_limit(dimension, value)
}

/// Parses an OSM length (`3.5`, `3,5 m`, `350 cm`, `12'6"`, `12 ft 6 in`)
/// into metres. Placeholder values (`none`, `default`, `below_default`,
/// `unsigned`) and lists return `None`.
pub(crate) fn parse_length_m(raw: &str) -> Option<f64> {
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .replace(['’', '′'], "'")
        .replace(['”', '″'], "\"")
        .replace("''", "\"");
    let quantities = quantities(&normalized)?;
    let metres = match quantities.as_slice() {
        [(value, unit)] => {
            value
                * match unit.as_str() {
                    "" | "m" | "meter" | "meters" | "metre" | "metres" => 1.0,
                    "cm" => 0.01,
                    "mm" => 0.001,
                    "ft" | "feet" | "foot" | "'" => METRES_PER_FOOT,
                    "in" | "inch" | "inches" | "\"" => METRES_PER_INCH,
                    _ => return None,
                }
        }
        [(feet, feet_unit), (inches, inch_unit)]
            if matches!(feet_unit.as_str(), "ft" | "feet" | "foot" | "'")
                && matches!(inch_unit.as_str(), "in" | "inch" | "inches" | "\"") =>
        {
            feet * METRES_PER_FOOT + inches * METRES_PER_INCH
        }
        _ => return None,
    };
    positive(metres)
}

/// Parses an OSM weight (`7.5`, `7,5 t`, `7500 kg`, `16000 lbs`, `10 st`)
/// into metric tonnes.
pub(crate) fn parse_weight_t(raw: &str) -> Option<f64> {
    let normalized = raw.trim().to_ascii_lowercase();
    let quantities = quantities(&normalized)?;
    let [(value, unit)] = quantities.as_slice() else {
        return None;
    };
    let tonnes = value
        * match unit.as_str() {
            "" | "t" | "ton" | "tons" | "tonne" | "tonnes" => 1.0,
            "kg" => 0.001,
            "lb" | "lbs" => TONNES_PER_POUND,
            "st" => TONNES_PER_SHORT_TON,
            "lt" => TONNES_PER_LONG_TON,
            _ => return None,
        };
    positive(tonnes)
}

fn positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Splits `12 ft 6 in` or `3,5m` into `(number, unit)` pairs. A decimal
/// comma is accepted when it is the only separator.
fn quantities(text: &str) -> Option<Vec<(f64, String)>> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut number = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || c == '.' || c == ',' {
                number.push(if c == ',' { '.' } else { c });
                chars.next();
            } else {
                break;
            }
        }
        if number.is_empty() || number.matches('.').count() > 1 {
            return None;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut unit = String::new();
        if let Some(&c) = chars.peek()
            && (c == '\'' || c == '"')
        {
            unit.push(c);
            chars.next();
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphabetic() {
                    unit.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
        }
        result.push((number.parse::<f64>().ok()?, unit));
    }
    (!result.is_empty()).then_some(result)
}

#[cfg(test)]
mod tests;
