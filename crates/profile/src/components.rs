use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentQuantity {
    Zero,
    TravelTime,
    Distance,
    Ascent,
    Descent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttributePredicate {
    pub(crate) key: String,
    pub(crate) expected: String,
    pub(crate) negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedCostComponent {
    pub(crate) quantity: ComponentQuantity,
    pub(crate) predicates: Vec<AttributePredicate>,
    pub(crate) overlay_name: Option<String>,
    pub(crate) invert_overlay: bool,
}

impl ParsedCostComponent {
    pub(crate) fn attribute_keys(&self) -> impl Iterator<Item = String> + '_ {
        self.predicates
            .iter()
            .map(|predicate| predicate.key.clone())
    }

    pub(crate) fn scales_with_travel_time(&self) -> bool {
        self.quantity == ComponentQuantity::TravelTime
    }

    pub(crate) fn edge_value(
        &self,
        travel_time_s: f64,
        distance_m: f64,
        ascent_m: f64,
        descent_m: f64,
        attribute_matches: &dyn Fn(&str, &str) -> bool,
    ) -> f64 {
        if self.predicates.iter().any(|predicate| {
            attribute_matches(&predicate.key, &predicate.expected) == predicate.negated
        }) {
            return 0.0;
        }
        match self.quantity {
            ComponentQuantity::Zero => 0.0,
            ComponentQuantity::TravelTime => travel_time_s,
            ComponentQuantity::Distance => distance_m,
            ComponentQuantity::Ascent => ascent_m,
            ComponentQuantity::Descent => descent_m,
        }
    }

    /// Value assumed when a static request does not attach a temporal
    /// overlay: direct overlay terms are zero and inverse terms are one.
    pub(crate) fn default_overlay_multiplier(&self) -> f64 {
        if self.overlay_name.is_none() || self.invert_overlay {
            1.0
        } else {
            0.0
        }
    }
}

pub(crate) fn parse_component_expression(expression: &str) -> Result<ParsedCostComponent> {
    let normalized = expression.replace(['×', '⋅'], "*").replace('−', "-");
    let factors = split_top_level(&normalized, '*')?;
    let mut quantity = None;
    let mut predicates = Vec::new();
    let mut overlay_name = None;
    let mut invert_overlay = false;

    for factor in factors {
        let factor = strip_wrapping_parentheses(factor.trim());
        let compact = factor
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let lower = compact.to_ascii_lowercase();
        let parsed_quantity = match lower.as_str() {
            "0" | "zero" => Some(ComponentQuantity::Zero),
            "travel_time" | "travel_time_s" | "time" => Some(ComponentQuantity::TravelTime),
            "distance" | "distance_m" | "length" | "length_m" => Some(ComponentQuantity::Distance),
            "ascent" | "ascent_m" => Some(ComponentQuantity::Ascent),
            "descent" | "descent_m" => Some(ComponentQuantity::Descent),
            _ => None,
        };
        if let Some(parsed_quantity) = parsed_quantity {
            if quantity.replace(parsed_quantity).is_some() {
                bail!("component expression contains more than one base quantity");
            }
            continue;
        }

        let (overlay, inverted) = if let Some(name) = lower.strip_prefix("overlay:") {
            (Some(name), false)
        } else if let Some(name) = lower.strip_prefix("1-overlay:") {
            (Some(name), true)
        } else {
            (None, false)
        };
        if let Some(overlay) = overlay {
            if overlay.is_empty() {
                bail!("component overlay name must not be empty");
            }
            if overlay_name.replace(overlay.to_string()).is_some() {
                bail!("component expression may reference at most one temporal overlay");
            }
            invert_overlay = inverted;
            continue;
        }

        predicates.push(
            parse_predicate(factor)
                .with_context(|| format!("unsupported component expression factor '{factor}'"))?,
        );
    }

    let quantity = quantity.context(
        "component expression must contain travel_time, distance, ascent, descent, or zero",
    )?;
    Ok(ParsedCostComponent {
        quantity,
        predicates,
        overlay_name,
        invert_overlay,
    })
}

fn parse_predicate(value: &str) -> Result<AttributePredicate> {
    let (operator, negated) = if value.contains("!=") {
        ("!=", true)
    } else if value.contains("==") {
        ("==", false)
    } else {
        bail!("expected an attribute comparison using == or !=");
    };
    let mut pieces = value.split(operator);
    let key = pieces.next().unwrap_or_default().trim();
    let expected = pieces.next().unwrap_or_default().trim();
    if key.is_empty() || expected.is_empty() || pieces.next().is_some() {
        bail!("attribute comparison must have exactly one value on each side");
    }
    let expected = expected
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            expected
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(expected);
    Ok(AttributePredicate {
        key: key.to_string(),
        expected: expected.to_string(),
        negated,
    })
}

fn split_top_level(value: &str, delimiter: char) -> Result<Vec<&str>> {
    let mut depth = 0_i32;
    let mut start = 0_usize;
    let mut parts = Vec::new();
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    bail!("component expression contains unmatched parentheses");
                }
            }
            character if character == delimiter && depth == 0 => {
                parts.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if depth != 0 {
        bail!("component expression contains unmatched parentheses");
    }
    parts.push(&value[start..]);
    if parts.iter().any(|part| part.trim().is_empty()) {
        bail!("component expression contains an empty factor");
    }
    Ok(parts)
}

fn strip_wrapping_parentheses(mut value: &str) -> &str {
    loop {
        let trimmed = value.trim();
        if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
            return trimmed;
        }
        let mut depth = 0_i32;
        let mut wraps_whole = true;
        for (index, character) in trimmed.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && index + 1 != trimmed.len() {
                        wraps_whole = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !wraps_whole {
            return trimmed;
        }
        value = &trimmed[1..trimmed.len() - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attribute_and_inverse_overlay_expressions() {
        let parsed = parse_component_expression(
            "travel_time × (covered == false) * (1 − overlay:shade_fraction)",
        )
        .expect("expression parses");
        assert_eq!(parsed.quantity, ComponentQuantity::TravelTime);
        assert_eq!(parsed.overlay_name.as_deref(), Some("shade_fraction"));
        assert!(parsed.invert_overlay);
        assert_eq!(parsed.predicates[0].key, "covered");
        assert_eq!(parsed.predicates[0].expected, "false");
        assert_eq!(
            parsed.edge_value(12.0, 20.0, 3.0, 0.0, &|key, value| {
                key == "covered" && value == "false"
            }),
            12.0
        );
    }

    #[test]
    fn rejects_multiple_base_quantities() {
        assert!(parse_component_expression("distance * ascent").is_err());
    }

    #[test]
    fn accepts_named_zero_for_facility_supplied_components() {
        let parsed = parse_component_expression("zero").expect("zero expression");
        assert_eq!(parsed.quantity, ComponentQuantity::Zero);
    }
}
