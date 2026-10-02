//! OSM conditional restriction values: `value @ condition; value @ (condition)`.
//! Conditions are time domains, vehicle-property comparisons
//! (`weight>7.5`), other qualifiers (`destination`, `wet`), or `AND`
//! combinations of them.

use netweevil_core::{TemporalEffect, TemporalRule, VehicleRestrictionKind};

use crate::opening_hours::{parse_osm_time_domain, strip_outer_parens};
use crate::vehicle_tags::conditional_limit;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConditionalEntry<'a> {
    pub(crate) value: &'a str,
    pub(crate) condition: Condition,
}

/// The supported condition shapes. Combinations (`Mo-Fr 07:00-09:00 AND
/// weight>7.5`) and qualifiers without a time or vehicle meaning are
/// [`Condition::Unsupported`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Condition {
    /// The condition's time domain, with effect `Closed` and every mode;
    /// callers replace both.
    Time(Vec<TemporalRule>),
    /// Vehicles exceeding a limit (`weight>7.5`).
    VehicleAbove(VehicleRestrictionKind),
    Unsupported(String),
}

impl Condition {
    /// The time domain rescoped to `effect` and `modes`.
    pub(crate) fn time_rules(
        &self,
        effect: TemporalEffect,
        modes: u16,
    ) -> Option<Vec<TemporalRule>> {
        let Condition::Time(rules) = self else {
            return None;
        };
        Some(
            rules
                .iter()
                .map(|rule| TemporalRule {
                    effect,
                    modes,
                    ..rule.clone()
                })
                .collect(),
        )
    }
}

/// Splits a conditional value into entries in tag order. Returns `None` for
/// a malformed value (an entry without `@`).
pub(crate) fn parse_conditional(raw: &str) -> Option<Vec<ConditionalEntry<'_>>> {
    let entries = split_top_level(raw, ';')
        .into_iter()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (value, condition) = entry.split_once('@')?;
            Some(ConditionalEntry {
                value: value.trim(),
                condition: parse_condition(condition.trim()),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!entries.is_empty()).then_some(entries)
}

fn parse_condition(text: &str) -> Condition {
    let text = strip_outer_parens(text);
    // A conjunction (`weight>7.5 AND Mo-Fr 07:00-19:00`, or two vehicle
    // properties) binds only vehicles matching every term; neither edge
    // schedules nor independent limits express that.
    if split_and(text).len() > 1 {
        return Condition::Unsupported(format!("unsupported combined condition '{text}'"));
    }
    if let Some(limit) = vehicle_comparison(text) {
        return Condition::VehicleAbove(limit);
    }
    match parse_osm_time_domain(text, TemporalEffect::Closed) {
        Ok(rules) => Condition::Time(rules),
        Err(error) => Condition::Unsupported(error),
    }
}

/// `weight>7.5`, `height > 3 m`, `axleload>=10`: a lower bound on a vehicle
/// property, which maps onto the matching maximum.
fn vehicle_comparison(term: &str) -> Option<VehicleRestrictionKind> {
    let term = strip_outer_parens(term.trim());
    let split = term.find(['>', '<', '='])?;
    let property = term[..split].trim().to_ascii_lowercase();
    let rest = &term[split..];
    let value = rest.strip_prefix(">=").or_else(|| rest.strip_prefix('>'))?;
    conditional_limit(&property, value.trim())
}

fn split_and(text: &str) -> Vec<&str> {
    let mut terms = Vec::new();
    let mut rest = text;
    while let Some(position) = rest.find(" AND ").or_else(|| rest.find(" and ")) {
        terms.push(rest[..position].trim());
        rest = &rest[position + 5..];
    }
    terms.push(rest.trim());
    terms
}

/// Splits on `separator` outside parentheses.
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == separator && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests;
