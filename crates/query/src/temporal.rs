use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use arrow_json::ArrayWriter;
use netweevil_core::{
    CompiledProfileBundle, EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, EVERY_DAY, FRIDAY, MONDAY,
    PUBLIC_HOLIDAY, SATURDAY, SUNDAY, THURSDAY, TUESDAY, TemporalEffect, TemporalRule,
    TemporalRuleSet, TopologyBundle, WEDNESDAY,
};
use netweevil_profile::ReturnGeometry;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, OffsetDateTime, Weekday};

use crate::{
    AlternativeRouteOptions, AnalysisOutcome, ConnectivityPolicy, FallbackPolicy, HopSelectionInfo,
    RouteResult, RouteSegment, RouteSummary, RoutingGraph, SearchStateKey, SnapOptions,
    SnappedPoint, build_breakdowns, build_route_geometry, hop_info_for_pair, no_route_failure,
    request_returns_detailed_path, snap_candidates_with_options, turn_penalty_seconds,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioOverlay {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub features: Vec<ScenarioFeatureOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioFeatureOverride {
    pub source_feature_id: i64,
    #[serde(default)]
    pub force_closed: bool,
    #[serde(default)]
    pub force_open: bool,
    /// Replace imported rules instead of appending scenario rules.
    #[serde(default)]
    pub replace_rules: bool,
    #[serde(default)]
    pub rules: Vec<TemporalRule>,
    /// Always-on multiplier applied after rule-based speed factors.
    #[serde(default)]
    pub speed_factor: Option<f32>,
}

pub fn load_scenario_overlay(path: impl AsRef<Path>) -> Result<ScenarioOverlay> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading scenario overlay {}", path.display()))?;
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON scenario overlay"),
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML scenario overlay")
        }
        other => bail!(
            "unsupported scenario overlay extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolidayCalendarDocument {
    #[serde(default)]
    pub dates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum HolidayCalendarFile {
    Document(HolidayCalendarDocument),
    Bare(Vec<String>),
}

pub fn load_holiday_calendar(path: impl AsRef<Path>) -> Result<HashSet<Date>> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading holiday calendar {}", path.display()))?;
    let file: HolidayCalendarFile = match path.extension().and_then(|extension| extension.to_str())
    {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON holiday calendar")?,
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML holiday calendar")?
        }
        other => bail!(
            "unsupported holiday calendar extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    };
    let values = match file {
        HolidayCalendarFile::Document(document) => document.dates,
        HolidayCalendarFile::Bare(values) => values,
    };
    values
        .into_iter()
        .map(|value| parse_date(&value).with_context(|| format!("invalid holiday date '{value}'")))
        .collect()
}

#[derive(Debug, Clone)]
struct TemporalOverlayInterval {
    start_unix_s: i64,
    end_unix_s: i64,
    values: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default)]
pub struct TemporalOverlaySeries {
    by_feature: HashMap<i64, Vec<TemporalOverlayInterval>>,
}

impl TemporalOverlaySeries {
    pub fn value(&self, feature_id: i64, timestamp: OffsetDateTime, name: &str) -> Option<f64> {
        let unix_s = timestamp.unix_timestamp();
        self.by_feature
            .get(&feature_id)?
            .iter()
            .find_map(|interval| {
                (unix_s >= interval.start_unix_s && unix_s < interval.end_unix_s)
                    .then(|| interval.values.get(name).copied())
                    .flatten()
            })
    }

    fn has_value_name(&self, feature_id: i64, name: &str) -> bool {
        self.by_feature.get(&feature_id).is_some_and(|intervals| {
            intervals
                .iter()
                .any(|interval| interval.values.contains_key(name))
        })
    }
}

pub fn load_temporal_overlay(path: impl AsRef<Path>) -> Result<TemporalOverlaySeries> {
    let path = path.as_ref();
    let rows = match path.extension().and_then(|extension| extension.to_str()) {
        Some("csv") => load_overlay_csv_rows(path)?,
        Some("parquet") => load_overlay_parquet_rows(path)?,
        other => bail!(
            "unsupported temporal overlay extension {:?}; use .csv or .parquet",
            other
        ),
    };
    overlay_series_from_rows(rows)
}

fn load_overlay_csv_rows(path: &Path) -> Result<Vec<BTreeMap<String, String>>> {
    let mut reader = csv::Reader::from_path(path)
        .with_context(|| format!("opening temporal overlay CSV {}", path.display()))?;
    let headers = reader.headers()?.clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        rows.push(
            headers
                .iter()
                .zip(record.iter())
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        );
    }
    Ok(rows)
}

fn load_overlay_parquet_rows(path: &Path) -> Result<Vec<BTreeMap<String, String>>> {
    let file = fs::File::open(path)
        .with_context(|| format!("opening temporal overlay Parquet {}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        let mut writer = ArrayWriter::new(Vec::new());
        writer.write(&batch)?;
        writer.finish()?;
        let values: Vec<serde_json::Map<String, serde_json::Value>> =
            serde_json::from_slice(&writer.into_inner())?;
        rows.extend(values.into_iter().map(|row| {
            row.into_iter()
                .map(|(key, value)| {
                    let value = value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string());
                    (key, value)
                })
                .collect()
        }));
    }
    Ok(rows)
}

fn overlay_series_from_rows(rows: Vec<BTreeMap<String, String>>) -> Result<TemporalOverlaySeries> {
    let mut by_feature: HashMap<i64, Vec<TemporalOverlayInterval>> = HashMap::new();
    for (index, mut row) in rows.into_iter().enumerate() {
        let feature_id = take_required(&mut row, &["feature_id", "source_feature_id"])?
            .parse::<i64>()
            .with_context(|| format!("overlay row {} has an invalid feature_id", index + 1))?;
        let start = parse_datetime(&take_required(&mut row, &["t_start", "start"])?)
            .with_context(|| format!("overlay row {} has an invalid t_start", index + 1))?;
        let end = parse_datetime(&take_required(&mut row, &["t_end", "end"])?)
            .with_context(|| format!("overlay row {} has an invalid t_end", index + 1))?;
        if end <= start {
            bail!("overlay row {} must end after it starts", index + 1);
        }
        let values = row
            .into_iter()
            .filter(|(_, value)| !value.trim().is_empty() && !value.eq_ignore_ascii_case("null"))
            .map(|(name, value)| {
                let parsed = value.parse::<f64>().with_context(|| {
                    format!("overlay row {} value '{name}' is not numeric", index + 1)
                })?;
                if !parsed.is_finite() {
                    bail!("overlay row {} value '{name}' must be finite", index + 1);
                }
                Ok((name, parsed))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        by_feature
            .entry(feature_id)
            .or_default()
            .push(TemporalOverlayInterval {
                start_unix_s: start.unix_timestamp(),
                end_unix_s: end.unix_timestamp(),
                values,
            });
    }
    for intervals in by_feature.values_mut() {
        intervals.sort_by_key(|interval| interval.start_unix_s);
    }
    Ok(TemporalOverlaySeries { by_feature })
}

fn take_required(row: &mut BTreeMap<String, String>, names: &[&str]) -> Result<String> {
    for name in names {
        if let Some(value) = row.remove(*name) {
            return Ok(value);
        }
    }
    bail!(
        "temporal overlay is missing required column '{}';",
        names[0]
    )
}

#[derive(Debug, Clone, Default)]
pub struct TemporalContext {
    pub scenario_id: Option<String>,
    scenario_features: HashMap<i64, ScenarioFeatureOverride>,
    pub holidays: HashSet<Date>,
    pub overlays: Vec<TemporalOverlaySeries>,
}

impl TemporalContext {
    pub fn new(
        scenario: Option<ScenarioOverlay>,
        holidays: HashSet<Date>,
        overlays: Vec<TemporalOverlaySeries>,
    ) -> Self {
        let scenario_id = scenario.as_ref().and_then(|scenario| scenario.id.clone());
        let scenario_features = scenario
            .map(|scenario| {
                scenario
                    .features
                    .into_iter()
                    .map(|feature| (feature.source_feature_id, feature))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            scenario_id,
            scenario_features,
            holidays,
            overlays,
        }
    }

    fn overlay_value(&self, feature_id: i64, timestamp: OffsetDateTime, name: &str) -> Option<f64> {
        self.overlays
            .iter()
            .rev()
            .find_map(|overlay| overlay.value(feature_id, timestamp, name))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeTemporalCost {
    pub(crate) wait_s: f64,
    pub(crate) travel_time_s: f64,
    pub(crate) generalized_cost: f64,
    pub(crate) components: BTreeMap<String, f64>,
    pub(crate) entry_time: OffsetDateTime,
    pub(crate) exit_time: OffsetDateTime,
}

pub(crate) fn evaluate_edge_at(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    edge_index: usize,
    requested_entry: OffsetDateTime,
    fraction: f64,
) -> Option<EdgeTemporalCost> {
    let routing_edge = topology.routing_edge(edge_index);
    let metric = metrics.edge_metrics.get(edge_index)?;
    let base_time_s = metric.travel_time_s?;
    let base_cost = metric.generalized_cost?;
    let scenario = context.scenario_features.get(&routing_edge.source_way_id);
    if scenario.is_some_and(|scenario| scenario.force_closed) {
        return None;
    }
    if routing_edge.flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION != 0
        && !scenario.is_some_and(scenario_enables_materialized_direction)
    {
        return None;
    }

    let rule_set = routing_edge
        .temporal_rule_id
        .and_then(|rule_id| topology.temporal_rule_sets.get(rule_id as usize));
    let rules_vary_over_time = has_time_varying_rules(rule_set, scenario);
    let speed_factor_varies = has_time_varying_speed_factor(rule_set, scenario);
    let rules = |timestamp| {
        evaluate_rules(
            rule_set,
            scenario,
            routing_edge.source_direction,
            timestamp,
            &context.holidays,
        )
    };
    let fraction = fraction.clamp(0.0, 1.0);
    let (actual_entry, speed_factor) = best_available_entry(
        requested_entry,
        metrics.temporal.allow_wait && !scenario.is_some_and(|value| value.force_closed),
        metrics.temporal.max_wait_s,
        base_time_s * fraction,
        rules_vary_over_time,
        speed_factor_varies,
        &rules,
    )?;
    let speed_factor = f64::from(speed_factor);
    if !speed_factor.is_finite() || speed_factor <= 0.0 {
        return None;
    }
    let wait_s = (actual_entry - requested_entry).as_seconds_f64();
    let travel_time_s = base_time_s * fraction / speed_factor;
    let mut generalized_cost = base_cost * fraction
        + (travel_time_s - base_time_s * fraction) * metrics.turn_costs.cost_time_weight
        + wait_s * metrics.turn_costs.cost_time_weight;
    let mut components = BTreeMap::new();
    for component in &metrics.components {
        let raw_value = f64::from(*component.edge_values.get(edge_index).unwrap_or(&0.0));
        let static_overlay_multiplier =
            if component.overlay_name.is_some() && !component.invert_overlay {
                0.0
            } else {
                1.0
            };
        let overlay_multiplier = component.overlay_name.as_deref().map_or(1.0, |name| {
            let value = context
                .overlay_value(routing_edge.source_way_id, actual_entry, name)
                .unwrap_or(0.0);
            if component.invert_overlay {
                1.0 - value
            } else {
                value
            }
        });
        let time_multiplier = if component.scales_with_travel_time {
            1.0 / speed_factor
        } else {
            1.0
        };
        let dynamic_value = raw_value * fraction * time_multiplier * overlay_multiplier;
        let static_value = raw_value * fraction * static_overlay_multiplier;
        generalized_cost += component.weight * (dynamic_value - static_value);
        components.insert(component.name.clone(), dynamic_value);
    }
    Some(EdgeTemporalCost {
        wait_s,
        travel_time_s,
        generalized_cost,
        components,
        entry_time: actual_entry,
        exit_time: actual_entry + Duration::seconds_f64(travel_time_s),
    })
}

fn best_available_entry(
    requested_entry: OffsetDateTime,
    allow_wait: bool,
    max_wait_s: f64,
    base_travel_time_s: f64,
    rules_vary_over_time: bool,
    speed_factor_varies: bool,
    rules: &impl Fn(OffsetDateTime) -> RuleEvaluation,
) -> Option<(OffsetDateTime, f32)> {
    let mut best = match rules(requested_entry) {
        RuleEvaluation::Available { speed_factor } => Some((
            requested_entry,
            speed_factor,
            requested_entry + Duration::seconds_f64(base_travel_time_s / f64::from(speed_factor)),
        )),
        RuleEvaluation::Unavailable => None,
    };
    if best.is_some() && !speed_factor_varies {
        return best.map(|(entry, speed_factor, _)| (entry, speed_factor));
    }
    if !allow_wait || !rules_vary_over_time || !max_wait_s.is_finite() || max_wait_s <= 0.0 {
        return best.map(|(entry, speed_factor, _)| (entry, speed_factor));
    }

    // Rule effects have minute resolution. Evaluating every future minute
    // boundary within the wait cap is therefore exhaustive, including
    // voluntary waiting for a faster SpeedFactor regime while the edge is
    // already open.
    let seconds_to_next_minute =
        if requested_entry.second() == 0 && requested_entry.nanosecond() == 0 {
            60
        } else {
            60 - i64::from(requested_entry.second())
        };
    let deadline = requested_entry + Duration::seconds_f64(max_wait_s);
    let mut candidate = requested_entry + Duration::seconds(seconds_to_next_minute);
    while candidate <= deadline {
        if let RuleEvaluation::Available { speed_factor } = rules(candidate) {
            let exit =
                candidate + Duration::seconds_f64(base_travel_time_s / f64::from(speed_factor));
            if best.as_ref().is_none_or(|(best_entry, _, best_exit)| {
                exit < *best_exit || (exit == *best_exit && candidate < *best_entry)
            }) {
                best = Some((candidate, speed_factor, exit));
            }
        }
        candidate += Duration::minutes(1);
    }
    best.map(|(entry, speed_factor, _)| (entry, speed_factor))
}

fn has_time_varying_rules(
    imported: Option<&TemporalRuleSet>,
    scenario: Option<&ScenarioFeatureOverride>,
) -> bool {
    if scenario.is_some_and(|scenario| scenario.force_open) {
        return false;
    }
    imported
        .filter(|_| !scenario.is_some_and(|scenario| scenario.replace_rules))
        .into_iter()
        .flat_map(|rules| &rules.rules)
        .chain(scenario.into_iter().flat_map(|scenario| &scenario.rules))
        .any(|rule| !rule.intervals.is_empty() || rule.day_mask != EVERY_DAY)
}

fn has_time_varying_speed_factor(
    imported: Option<&TemporalRuleSet>,
    scenario: Option<&ScenarioFeatureOverride>,
) -> bool {
    if scenario.is_some_and(|scenario| scenario.force_open) {
        return false;
    }
    imported
        .filter(|_| !scenario.is_some_and(|scenario| scenario.replace_rules))
        .into_iter()
        .flat_map(|rules| &rules.rules)
        .chain(scenario.into_iter().flat_map(|scenario| &scenario.rules))
        .any(|rule| {
            matches!(rule.effect, TemporalEffect::SpeedFactor(_))
                && (!rule.intervals.is_empty() || rule.day_mask != EVERY_DAY)
        })
}

fn scenario_enables_materialized_direction(scenario: &ScenarioFeatureOverride) -> bool {
    scenario.force_open
        || scenario.rules.iter().any(|rule| {
            matches!(
                rule.effect,
                TemporalEffect::ForwardOnly | TemporalEffect::BackwardOnly
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RuleEvaluation {
    Available { speed_factor: f32 },
    Unavailable,
}

fn evaluate_rules(
    imported: Option<&TemporalRuleSet>,
    scenario: Option<&ScenarioFeatureOverride>,
    source_direction: i8,
    timestamp: OffsetDateTime,
    holidays: &HashSet<Date>,
) -> RuleEvaluation {
    if scenario.is_some_and(|scenario| scenario.force_open) {
        return RuleEvaluation::Available {
            speed_factor: scenario.and_then(|value| value.speed_factor).unwrap_or(1.0),
        };
    }
    let imported_rules = imported
        .filter(|_| !scenario.is_some_and(|scenario| scenario.replace_rules))
        .into_iter()
        .flat_map(|rules| &rules.rules);
    let scenario_rules = scenario.into_iter().flat_map(|scenario| &scenario.rules);
    let rules = imported_rules.chain(scenario_rules).collect::<Vec<_>>();
    let has_open_only = rules
        .iter()
        .any(|rule| matches!(rule.effect, TemporalEffect::OpenOnly));
    let mut open_only_active = false;
    let mut speed_factor = scenario.and_then(|value| value.speed_factor).unwrap_or(1.0);
    for rule in rules {
        let active = rule_active(rule, timestamp, holidays);
        match rule.effect {
            TemporalEffect::OpenOnly => open_only_active |= active,
            TemporalEffect::Closed if active => return RuleEvaluation::Unavailable,
            TemporalEffect::ForwardOnly if active && source_direction < 0 => {
                return RuleEvaluation::Unavailable;
            }
            TemporalEffect::BackwardOnly if active && source_direction > 0 => {
                return RuleEvaluation::Unavailable;
            }
            TemporalEffect::SpeedFactor(factor) if active => speed_factor *= factor,
            _ => {}
        }
    }
    if has_open_only && !open_only_active || !speed_factor.is_finite() || speed_factor <= 0.0 {
        RuleEvaluation::Unavailable
    } else {
        RuleEvaluation::Available { speed_factor }
    }
}

fn rule_active(rule: &TemporalRule, timestamp: OffsetDateTime, holidays: &HashSet<Date>) -> bool {
    let minute = timestamp.hour() as u16 * 60 + timestamp.minute() as u16;
    if rule.intervals.is_empty() {
        return day_matches(rule.day_mask, timestamp.date(), holidays);
    }
    rule.intervals.iter().any(|interval| {
        if interval.start_minute <= interval.end_minute {
            return day_matches(rule.day_mask, timestamp.date(), holidays)
                && interval.contains(minute);
        }

        // A wrapping interval belongs to the day on which it starts. Thus a
        // Friday 22:00-06:00 rule uses Friday's mask both late on Friday and
        // early on Saturday; checking Saturday's mask after midnight would
        // shift the rule to the wrong day.
        if minute >= interval.start_minute {
            day_matches(rule.day_mask, timestamp.date(), holidays)
        } else if minute < interval.end_minute {
            day_matches(
                rule.day_mask,
                (timestamp - Duration::days(1)).date(),
                holidays,
            )
        } else {
            false
        }
    })
}

fn day_matches(day_mask: u8, date: Date, holidays: &HashSet<Date>) -> bool {
    let day_bit = if holidays.contains(&date) {
        PUBLIC_HOLIDAY
    } else {
        weekday_bit(date.weekday())
    };
    day_mask & day_bit != 0
}

#[cfg(test)]
fn next_available_entry(
    requested_entry: OffsetDateTime,
    max_wait_s: f64,
    rules: &impl Fn(OffsetDateTime) -> RuleEvaluation,
) -> Option<(OffsetDateTime, f32)> {
    if !max_wait_s.is_finite() || max_wait_s <= 0.0 {
        return None;
    }
    let seconds_to_next_minute =
        if requested_entry.second() == 0 && requested_entry.nanosecond() == 0 {
            60
        } else {
            60 - i64::from(requested_entry.second())
        };
    let mut candidate = requested_entry + Duration::seconds(seconds_to_next_minute);
    let deadline = requested_entry + Duration::seconds_f64(max_wait_s);
    while candidate <= deadline {
        if let RuleEvaluation::Available { speed_factor } = rules(candidate) {
            return Some((candidate, speed_factor));
        }
        candidate += Duration::minutes(1);
    }
    None
}

pub fn parse_datetime(value: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value.trim(), &Rfc3339)
        .with_context(|| "datetime must be RFC 3339 with an explicit UTC offset")
}

fn parse_date(value: &str) -> Result<Date> {
    let mut fields = value.trim().split('-');
    let year = fields.next().context("date must use YYYY-MM-DD")?.parse()?;
    let month = fields
        .next()
        .context("date must use YYYY-MM-DD")?
        .parse::<u8>()?;
    let day = fields.next().context("date must use YYYY-MM-DD")?.parse()?;
    if fields.next().is_some() {
        bail!("date must use YYYY-MM-DD");
    }
    Date::from_calendar_date(year, time::Month::try_from(month)?, day)
        .context("date must use YYYY-MM-DD")
}

fn weekday_bit(weekday: Weekday) -> u8 {
    match weekday {
        Weekday::Monday => MONDAY,
        Weekday::Tuesday => TUESDAY,
        Weekday::Wednesday => WEDNESDAY,
        Weekday::Thursday => THURSDAY,
        Weekday::Friday => FRIDAY,
        Weekday::Saturday => SATURDAY,
        Weekday::Sunday => SUNDAY,
    }
}

pub fn everyday_mask() -> u8 {
    EVERY_DAY
}

pub(crate) fn context_from_request(
    options: &crate::TemporalRequestOptions,
) -> Result<TemporalContext> {
    let scenario = options
        .scenario
        .as_ref()
        .map(load_scenario_overlay)
        .transpose()?;
    let holidays = options
        .holiday_calendar
        .as_ref()
        .map(load_holiday_calendar)
        .transpose()?
        .unwrap_or_default();
    let overlays = options
        .overlays
        .iter()
        .map(load_temporal_overlay)
        .collect::<Result<Vec<_>>>()?;
    Ok(TemporalContext::new(scenario, holidays, overlays))
}

pub(crate) fn validate_speed_factor_wait_policy(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
) -> Result<()> {
    let rule_has_varying_speed = |rule: &TemporalRule| {
        matches!(rule.effect, TemporalEffect::SpeedFactor(_))
            && (!rule.intervals.is_empty() || rule.day_mask != EVERY_DAY)
    };
    if !topology
        .temporal_rule_sets
        .iter()
        .flat_map(|rules| &rules.rules)
        .any(rule_has_varying_speed)
        && !context
            .scenario_features
            .values()
            .flat_map(|scenario| &scenario.rules)
            .any(rule_has_varying_speed)
    {
        return Ok(());
    }
    let mut speed_factor_edges = Vec::new();
    for edge_index in 0..topology.edge_count() {
        if metrics
            .edge_metrics
            .get(edge_index)
            .and_then(|metric| metric.travel_time_s)
            .is_none()
        {
            continue;
        }
        let edge = topology.routing_edge(edge_index);
        let scenario = context.scenario_features.get(&edge.source_way_id);
        let imported = edge
            .temporal_rule_id
            .and_then(|rule_id| topology.temporal_rule_sets.get(rule_id as usize));
        if has_time_varying_speed_factor(imported, scenario) {
            speed_factor_edges.push((edge_index, edge.source_way_id));
        }
    }
    let Some(&(_, first_feature)) = speed_factor_edges.first() else {
        return Ok(());
    };
    if !metrics.temporal.allow_wait || metrics.temporal.max_wait_s <= 0.0 {
        bail!(
            "temporal SpeedFactor rules on source feature {first_feature} require temporal.allow_wait=true and temporal.max_wait_s > 0 so faster future regimes can be traversed with FIFO voluntary waiting"
        );
    }
    for component in &metrics.components {
        let has_active_overlay_values = component.overlay_name.as_deref().is_some_and(|name| {
            speed_factor_edges.iter().any(|(edge_index, feature_id)| {
                component
                    .edge_values
                    .get(*edge_index)
                    .is_some_and(|value| *value != 0.0)
                    && context
                        .overlays
                        .iter()
                        .any(|overlay| overlay.has_value_name(*feature_id, name))
            })
        });
        if has_active_overlay_values {
            bail!(
                "temporal SpeedFactor rules on source feature {first_feature} cannot use single-choice voluntary waiting because overlay-backed component '{}' is entry-time dependent on that feature; remove the overlapping overlay value or model explicit traversal alternatives",
                component.name
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct TemporalTraversal {
    edge_index: usize,
    fraction: f64,
    turn_time_s: f64,
    cost: EdgeTemporalCost,
}

#[derive(Debug, Clone)]
struct TemporalLabel {
    state: SearchStateKey,
    arrival: OffsetDateTime,
    generalized_cost: f64,
    previous: Option<usize>,
    traversal: TemporalTraversal,
    active: bool,
}

#[derive(Debug, Clone)]
struct EarliestArrivalLabel {
    state: SearchStateKey,
    arrival: OffsetDateTime,
    generalized_cost: f64,
    previous: Option<usize>,
    traversal: TemporalTraversal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EarliestArrivalHeapEntry {
    label_id: usize,
    arrival: OffsetDateTime,
}

impl PartialOrd for EarliestArrivalHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EarliestArrivalHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .arrival
            .cmp(&self.arrival)
            .then_with(|| other.label_id.cmp(&self.label_id))
    }
}

#[derive(Debug, Clone, Copy)]
struct TemporalHeapEntry {
    label_id: usize,
    generalized_cost: f64,
}

impl PartialEq for TemporalHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.generalized_cost.to_bits() == other.generalized_cost.to_bits()
            && self.label_id == other.label_id
    }
}

impl Eq for TemporalHeapEntry {}

impl PartialOrd for TemporalHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TemporalHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .generalized_cost
            .total_cmp(&self.generalized_cost)
            .then_with(|| other.label_id.cmp(&self.label_id))
    }
}

#[derive(Debug, Clone)]
struct TemporalCandidatePath {
    origin: SnappedPoint,
    destination: SnappedPoint,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    generalized_cost: f64,
    traversals: Vec<TemporalTraversal>,
    hop_info: HopSelectionInfo,
}

pub(crate) fn execute_temporal_route_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &crate::RouteRequest,
    context: &TemporalContext,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if request.temporal.departure_time.is_some() && request.temporal.arrive_by.is_some() {
        bail!("route request must set only one of departure_time or arrive_by");
    }
    if request.temporal.max_labels_per_state == 0 {
        bail!("max_labels_per_state must be greater than zero");
    }
    if request.fallback != FallbackPolicy::default() {
        bail!("time-dependent routing currently requires the strict fallback policy");
    }
    validate_speed_factor_wait_policy(topology, metrics, context)?;
    let temporal_routing_graph;
    let routing_graph = if routing_graph.includes_temporal_materialized_directions {
        routing_graph
    } else {
        temporal_routing_graph = crate::build_temporal_routing_graph(topology, metrics)?;
        &temporal_routing_graph
    };
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let search_snap = SnapOptions {
        max_distance_m: search_distance_m,
        ..request.snap.clone()
    };
    let origin_candidates =
        snap_candidates_with_options(topology, routing_graph, &request.origin, &search_snap, true)?;
    let destination_candidates = snap_candidates_with_options(
        topology,
        routing_graph,
        &request.destination,
        &search_snap,
        false,
    )?;

    let candidate = if let Some(departure) = request.temporal.departure_time.as_deref() {
        temporal_route_between_candidates(
            topology,
            metrics,
            routing_graph,
            &request.connectivity,
            request.snap.max_distance_m,
            &origin_candidates,
            &destination_candidates,
            parse_datetime(departure)?,
            request.temporal.max_labels_per_state,
            context,
        )?
    } else if let Some(arrive_by) = request.temporal.arrive_by.as_deref() {
        let deadline = parse_datetime(arrive_by)?;
        arrive_by_route_between_candidates(
            topology,
            metrics,
            routing_graph,
            &request.connectivity,
            request.snap.max_distance_m,
            &origin_candidates,
            &destination_candidates,
            deadline,
            request.temporal.arrive_by_lookback_s,
            request.temporal.max_labels_per_state,
            context,
        )?
    } else {
        bail!("a scenario or temporal overlay requires departure_time or arrive_by");
    };

    let candidate = candidate.ok_or_else(|| {
        anyhow::Error::new(no_route_failure(
            topology,
            &origin_candidates,
            &destination_candidates,
        ))
    })?;
    build_temporal_route_result(topology, metrics, request, context, candidate, edge_names)
}

fn arrive_by_route_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    connectivity: &ConnectivityPolicy,
    snap_max_distance_m: f64,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    deadline: OffsetDateTime,
    lookback_s: f64,
    max_labels_per_state: usize,
    context: &TemporalContext,
) -> Result<Option<TemporalCandidatePath>> {
    if !lookback_s.is_finite() || lookback_s <= 0.0 {
        bail!("arrive_by_lookback_s must be finite and greater than zero");
    }
    let earliest = deadline - Duration::seconds_f64(lookback_s);
    let mut best: Option<TemporalCandidatePath> = None;
    for origin in origin_candidates {
        for destination in destination_candidates {
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let Some((departure, arrival, generalized_cost, traversals)) =
                latest_temporal_route_for_pair(
                    topology,
                    metrics,
                    routing_graph,
                    origin,
                    destination,
                    earliest,
                    deadline,
                    max_labels_per_state,
                    context,
                    None,
                )?
            else {
                continue;
            };
            if best.as_ref().is_none_or(|candidate| {
                departure > candidate.departure
                    || (departure == candidate.departure
                        && generalized_cost < candidate.generalized_cost)
            }) {
                best = Some(TemporalCandidatePath {
                    origin: origin.clone(),
                    destination: destination.clone(),
                    departure,
                    arrival,
                    generalized_cost,
                    traversals,
                    hop_info,
                });
            }
        }
    }
    Ok(best)
}

/// Returns exact feasible departure-window endpoints over the union of all
/// time-window paths, newest first. Component/Pareto routing tests these
/// endpoints in order so constraints cannot make it regress to sampling
/// departure instants around narrow openings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exact_arrive_by_departure_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    connectivity: &ConnectivityPolicy,
    snap_max_distance_m: f64,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    deadline: OffsetDateTime,
    lookback_s: f64,
    max_labels_per_state: usize,
    context: &TemporalContext,
) -> Result<Vec<OffsetDateTime>> {
    if !lookback_s.is_finite() || lookback_s <= 0.0 {
        bail!("arrive_by_lookback_s must be finite and greater than zero");
    }
    let earliest = deadline - Duration::seconds_f64(lookback_s);
    let mut departures = Vec::new();
    for origin in origin_candidates {
        for destination in destination_candidates {
            if hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity).is_none() {
                continue;
            }
            let _ = latest_temporal_route_for_pair(
                topology,
                metrics,
                routing_graph,
                origin,
                destination,
                earliest,
                deadline,
                max_labels_per_state,
                context,
                Some(&mut departures),
            )?;
        }
    }
    departures.sort_by(|left, right| right.cmp(left));
    departures.dedup();
    Ok(departures)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimeWindow {
    start: OffsetDateTime,
    end: OffsetDateTime,
}

impl TimeWindow {
    fn intersect(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start <= end).then_some(Self { start, end })
    }

    fn contains(self, value: OffsetDateTime) -> bool {
        value >= self.start && value <= self.end
    }

    fn shifted(self, duration: Duration) -> Self {
        Self {
            start: self.start + duration,
            end: self.end + duration,
        }
    }

    fn contains_window(self, other: Self) -> bool {
        self.start <= other.start && self.end >= other.end
    }
}

#[derive(Debug, Clone)]
enum EdgeArrivalSegment {
    Affine {
        entries: TimeWindow,
        duration: Duration,
    },
    Constant {
        entries: TimeWindow,
        actual_entry: OffsetDateTime,
        exit: OffsetDateTime,
    },
}

#[derive(Debug, Clone)]
struct ReverseTemporalLabel {
    state: SearchStateKey,
    window: TimeWindow,
    exit_window: TimeWindow,
    fraction: f64,
    next: Option<usize>,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReverseTemporalHeapEntry {
    label_id: usize,
    upper_bound: OffsetDateTime,
}

impl PartialOrd for ReverseTemporalHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReverseTemporalHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.upper_bound
            .cmp(&other.upper_bound)
            .then_with(|| other.label_id.cmp(&self.label_id))
    }
}

#[allow(clippy::too_many_arguments)]
fn latest_temporal_route_for_pair(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    earliest: OffsetDateTime,
    deadline: OffsetDateTime,
    max_labels_per_state: usize,
    context: &TemporalContext,
    mut departure_candidates: Option<&mut Vec<OffsetDateTime>>,
) -> Result<Option<(OffsetDateTime, OffsetDateTime, f64, Vec<TemporalTraversal>)>> {
    if origin.snapped_edge_id.is_none()
        && destination.snapped_edge_id.is_none()
        && origin.snapped_node_id == destination.snapped_node_id
    {
        if let Some(candidates) = departure_candidates.as_deref_mut() {
            candidates.push(deadline);
        }
        return Ok(Some((deadline, deadline, 0.0, Vec::new())));
    }
    let range = TimeWindow {
        start: earliest,
        end: deadline,
    };
    let mut segment_cache = HashMap::<(usize, u64), Vec<EdgeArrivalSegment>>::new();
    let overlay_names = metrics
        .components
        .iter()
        .filter_map(|component| component.overlay_name.clone())
        .collect::<HashSet<_>>();

    let collect_all_departures = departure_candidates.is_some();
    let mut best: Option<(OffsetDateTime, OffsetDateTime, f64, Vec<TemporalTraversal>)> = None;
    if let (
        Some(origin_edge),
        Some(origin_fraction),
        Some(destination_edge),
        Some(destination_fraction),
    ) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) && origin_edge == destination_edge
        && origin_fraction <= destination_fraction
    {
        let fraction = destination_fraction - origin_fraction;
        let windows = edge_entry_windows_for_exit(
            topology,
            metrics,
            context,
            origin_edge as usize,
            fraction,
            range,
            range,
            &mut segment_cache,
        );
        for window in windows.into_iter().rev() {
            if let Some(candidates) = departure_candidates.as_deref_mut() {
                candidates.push(window.end);
                collect_path_overlay_departure_candidates(
                    topology,
                    metrics,
                    context,
                    &[(origin_edge as usize, fraction)],
                    window,
                    range,
                    &overlay_names,
                    &mut segment_cache,
                    candidates,
                );
            }
            if let Some(result) = replay_temporal_edge_path(
                topology,
                metrics,
                context,
                &[(origin_edge as usize, fraction)],
                window.end,
                deadline,
            ) && best
                .as_ref()
                .is_none_or(|best| result.0 > best.0 || (result.0 == best.0 && result.2 < best.2))
            {
                best = Some(result);
            }
        }
    }

    let mut labels = Vec::<ReverseTemporalLabel>::new();
    let mut state_labels = HashMap::<SearchStateKey, Vec<usize>>::new();
    let mut heap = BinaryHeap::<ReverseTemporalHeapEntry>::new();
    let destination_specs = if let (Some(edge), Some(fraction)) = (
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) {
        vec![(edge as usize, fraction)]
    } else {
        routing_graph
            .incoming_edges(destination.snapped_node_id as usize)
            .iter()
            .map(|edge| (*edge as usize, 1.0))
            .collect()
    };
    for (edge_index, fraction) in destination_specs {
        let windows = edge_entry_windows_for_exit(
            topology,
            metrics,
            context,
            edge_index,
            fraction,
            range,
            range,
            &mut segment_cache,
        );
        let reverse_state = routing_graph.reverse_automaton.transition(0, edge_index);
        for window in windows {
            let label = ReverseTemporalLabel {
                state: SearchStateKey {
                    edge_index,
                    automaton_state: reverse_state,
                },
                window,
                exit_window: range,
                fraction,
                next: None,
                active: true,
            };
            insert_reverse_temporal_label(
                &mut labels,
                &mut state_labels,
                &mut heap,
                label,
                origin,
                max_labels_per_state,
            )?;
        }
    }

    while let Some(entry) = heap.pop() {
        if !labels[entry.label_id].active {
            continue;
        }
        if !collect_all_departures
            && best
                .as_ref()
                .is_some_and(|candidate| entry.upper_bound < candidate.0)
        {
            break;
        }
        let label = labels[entry.label_id].clone();
        let origin_windows = origin_windows_for_reverse_label(
            topology,
            metrics,
            context,
            origin,
            &label,
            range,
            &mut segment_cache,
        );
        let mut matched_origin = false;
        for (window, first_fraction) in origin_windows.into_iter().rev() {
            matched_origin = true;
            if let Some(candidates) = departure_candidates.as_deref_mut() {
                candidates.push(window.end);
            }
            let path = reverse_label_edge_path(&labels, entry.label_id, first_fraction);
            if let Some(candidates) = departure_candidates.as_deref_mut() {
                collect_path_overlay_departure_candidates(
                    topology,
                    metrics,
                    context,
                    &path,
                    window,
                    range,
                    &overlay_names,
                    &mut segment_cache,
                    candidates,
                );
            }
            if let Some(candidate) =
                replay_temporal_edge_path(topology, metrics, context, &path, window.end, deadline)
                && best.as_ref().is_none_or(|best| {
                    candidate.0 > best.0 || (candidate.0 == best.0 && candidate.2 < best.2)
                })
            {
                best = Some(candidate);
            }
        }
        if matched_origin {
            continue;
        }

        for transition_index in routing_graph.reverse_transition_range(label.state.edge_index) {
            let previous_edge = routing_graph.reverse_transition_edges[transition_index] as usize;
            if !routing_graph
                .reverse_automaton
                .is_transition_allowed(label.state.automaton_state, previous_edge)
            {
                continue;
            }
            let turn_s =
                turn_penalty_seconds(topology, metrics, previous_edge, label.state.edge_index);
            let exit_window = label.window.shifted(-Duration::seconds_f64(turn_s));
            let windows = edge_entry_windows_for_exit(
                topology,
                metrics,
                context,
                previous_edge,
                1.0,
                exit_window,
                range,
                &mut segment_cache,
            );
            let reverse_state = routing_graph
                .reverse_automaton
                .transition(label.state.automaton_state, previous_edge);
            for window in windows {
                let previous = ReverseTemporalLabel {
                    state: SearchStateKey {
                        edge_index: previous_edge,
                        automaton_state: reverse_state,
                    },
                    window,
                    exit_window,
                    fraction: 1.0,
                    next: Some(entry.label_id),
                    active: true,
                };
                insert_reverse_temporal_label(
                    &mut labels,
                    &mut state_labels,
                    &mut heap,
                    previous,
                    origin,
                    max_labels_per_state,
                )?;
            }
        }
    }
    Ok(best)
}

fn origin_windows_for_reverse_label(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    origin: &SnappedPoint,
    label: &ReverseTemporalLabel,
    range: TimeWindow,
    segment_cache: &mut HashMap<(usize, u64), Vec<EdgeArrivalSegment>>,
) -> Vec<(TimeWindow, f64)> {
    if let (Some(origin_edge), Some(origin_fraction)) =
        (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        if origin_edge as usize != label.state.edge_index {
            return Vec::new();
        }
        let fraction = 1.0 - origin_fraction;
        return edge_entry_windows_for_exit(
            topology,
            metrics,
            context,
            label.state.edge_index,
            fraction,
            label.exit_window,
            range,
            segment_cache,
        )
        .into_iter()
        .map(|window| (window, fraction))
        .collect();
    }
    let edge = topology.routing_edge(label.state.edge_index);
    (edge.from.0 == origin.snapped_node_id)
        .then_some(vec![(label.window, label.fraction)])
        .unwrap_or_default()
}

fn reverse_label_edge_path(
    labels: &[ReverseTemporalLabel],
    first_label: usize,
    first_fraction: f64,
) -> Vec<(usize, f64)> {
    let mut path = Vec::new();
    let mut cursor = Some(first_label);
    let mut first = true;
    while let Some(label_id) = cursor {
        let label = &labels[label_id];
        path.push((
            label.state.edge_index,
            if first {
                first_fraction
            } else {
                label.fraction
            },
        ));
        first = false;
        cursor = label.next;
    }
    path
}

/// Adds the exact departure instants at which a component overlay can change
/// along this path. Overlay intervals are evaluated at an edge's *actual*
/// entry time, so a boundary must first be mapped to requested edge-entry
/// windows (including FIFO waits) and then pulled backward through every
/// prefix edge and turn. Testing both half-open sides prevents a budget from
/// skipping the last feasible nanosecond before an overlay change.
#[allow(clippy::too_many_arguments)]
fn collect_path_overlay_departure_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    path: &[(usize, f64)],
    feasible_departures: TimeWindow,
    range: TimeWindow,
    overlay_names: &HashSet<String>,
    segment_cache: &mut HashMap<(usize, u64), Vec<EdgeArrivalSegment>>,
    candidates: &mut Vec<OffsetDateTime>,
) {
    if overlay_names.is_empty() {
        return;
    }
    for (position, &(edge_index, fraction)) in path.iter().enumerate() {
        let feature_id = topology.routing_edge(edge_index).source_way_id;
        for boundary in overlay_boundaries_for_feature(context, feature_id, overlay_names, range) {
            for requested_entry in requested_entry_candidates_around_actual_boundary(
                topology,
                metrics,
                context,
                edge_index,
                fraction,
                boundary,
                range,
                segment_cache,
            ) {
                let target = TimeWindow {
                    start: requested_entry,
                    end: requested_entry,
                };
                for departure_window in departure_windows_for_path_entry(
                    topology,
                    metrics,
                    context,
                    path,
                    position,
                    target,
                    range,
                    segment_cache,
                ) {
                    if let Some(window) = departure_window.intersect(feasible_departures) {
                        candidates.push(window.end);
                    }
                }
            }
        }
    }
}

fn overlay_boundaries_for_feature(
    context: &TemporalContext,
    feature_id: i64,
    overlay_names: &HashSet<String>,
    range: TimeWindow,
) -> Vec<OffsetDateTime> {
    let mut boundaries = Vec::new();
    for overlay in &context.overlays {
        let Some(intervals) = overlay.by_feature.get(&feature_id) else {
            continue;
        };
        for interval in intervals {
            if !interval
                .values
                .keys()
                .any(|name| overlay_names.contains(name))
            {
                continue;
            }
            for unix_s in [interval.start_unix_s, interval.end_unix_s] {
                if let Ok(boundary) = OffsetDateTime::from_unix_timestamp(unix_s) {
                    let boundary = boundary.to_offset(range.start.offset());
                    if range.contains(boundary) {
                        boundaries.push(boundary);
                    }
                }
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

#[allow(clippy::too_many_arguments)]
fn requested_entry_candidates_around_actual_boundary(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    edge_index: usize,
    fraction: f64,
    boundary: OffsetDateTime,
    range: TimeWindow,
    segment_cache: &mut HashMap<(usize, u64), Vec<EdgeArrivalSegment>>,
) -> Vec<OffsetDateTime> {
    let fraction = fraction.clamp(0.0, 1.0);
    let segments = segment_cache
        .entry((edge_index, fraction.to_bits()))
        .or_insert_with(|| {
            build_edge_arrival_segments(topology, metrics, context, edge_index, fraction, range)
        });
    let before_boundary = boundary - Duration::nanoseconds(1);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for segment in segments {
        match segment {
            EdgeArrivalSegment::Affine { entries, .. } => {
                if entries.start < boundary {
                    let window = TimeWindow {
                        start: entries.start,
                        end: entries.end.min(before_boundary),
                    };
                    if window.start <= window.end {
                        before.push(window);
                    }
                }
                if entries.end >= boundary {
                    let window = TimeWindow {
                        start: entries.start.max(boundary),
                        end: entries.end,
                    };
                    if window.start <= window.end {
                        after.push(window);
                    }
                }
            }
            EdgeArrivalSegment::Constant {
                entries,
                actual_entry,
                ..
            } => {
                if *actual_entry < boundary {
                    before.push(*entries);
                } else {
                    after.push(*entries);
                }
            }
        }
    }
    let mut candidates = merge_time_windows(before)
        .into_iter()
        .chain(merge_time_windows(after))
        .map(|window| window.end)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

#[allow(clippy::too_many_arguments)]
fn departure_windows_for_path_entry(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    path: &[(usize, f64)],
    edge_position: usize,
    requested_entry: TimeWindow,
    range: TimeWindow,
    segment_cache: &mut HashMap<(usize, u64), Vec<EdgeArrivalSegment>>,
) -> Vec<TimeWindow> {
    let mut entry_windows = vec![requested_entry];
    for position in (1..=edge_position).rev() {
        let (current_edge, _) = path[position];
        let (previous_edge, previous_fraction) = path[position - 1];
        let turn_s = turn_penalty_seconds(topology, metrics, previous_edge, current_edge);
        let mut previous_entries = Vec::new();
        for current_entry in entry_windows {
            previous_entries.extend(edge_entry_windows_for_exit(
                topology,
                metrics,
                context,
                previous_edge,
                previous_fraction,
                current_entry.shifted(-Duration::seconds_f64(turn_s)),
                range,
                segment_cache,
            ));
        }
        entry_windows = merge_time_windows(previous_entries);
        if entry_windows.is_empty() {
            break;
        }
    }
    entry_windows
}

fn replay_temporal_edge_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    path: &[(usize, f64)],
    departure: OffsetDateTime,
    deadline: OffsetDateTime,
) -> Option<(OffsetDateTime, OffsetDateTime, f64, Vec<TemporalTraversal>)> {
    let mut arrival = departure;
    let mut generalized_cost = 0.0;
    let mut traversals = Vec::with_capacity(path.len());
    let mut previous = None;
    for &(edge_index, fraction) in path {
        let turn_time_s = previous.map_or(0.0, |previous_edge| {
            turn_penalty_seconds(topology, metrics, previous_edge, edge_index)
        });
        let requested_entry = arrival + Duration::seconds_f64(turn_time_s);
        let cost = evaluate_edge_at(
            topology,
            metrics,
            context,
            edge_index,
            requested_entry,
            fraction,
        )?;
        generalized_cost +=
            turn_time_s * metrics.turn_costs.cost_time_weight + cost.generalized_cost;
        arrival = cost.exit_time;
        traversals.push(TemporalTraversal {
            edge_index,
            fraction,
            turn_time_s,
            cost,
        });
        previous = Some(edge_index);
    }
    (arrival <= deadline).then_some((departure, arrival, generalized_cost, traversals))
}

fn insert_reverse_temporal_label(
    labels: &mut Vec<ReverseTemporalLabel>,
    state_labels: &mut HashMap<SearchStateKey, Vec<usize>>,
    heap: &mut BinaryHeap<ReverseTemporalHeapEntry>,
    label: ReverseTemporalLabel,
    origin: &SnappedPoint,
    max_labels: usize,
) -> Result<()> {
    let state = label.state;
    let existing = state_labels.entry(state).or_default();
    existing.retain(|label_id| labels[*label_id].active);
    // Feasibility containment alone is not a valid dominance rule here: two
    // suffix paths can admit the same latest departure but have different
    // generalized costs. Only merge windows for the identical suffix path.
    // Distinct suffixes are retained until the explicit label guard, which
    // fails loudly instead of silently returning a non-optimal route.
    let same_suffix = |existing: &ReverseTemporalLabel| {
        existing.next == label.next && existing.fraction.to_bits() == label.fraction.to_bits()
    };
    if existing.iter().any(|label_id| {
        let existing = &labels[*label_id];
        same_suffix(existing) && existing.window.contains_window(label.window)
    }) {
        return Ok(());
    }
    for &label_id in existing.iter() {
        if same_suffix(&labels[label_id]) && label.window.contains_window(labels[label_id].window) {
            labels[label_id].active = false;
        }
    }
    existing.retain(|label_id| labels[*label_id].active);
    if existing.len() >= max_labels {
        bail!(
            "exact arrive-by search exceeded max_labels_per_state={max_labels} at edge {}; increase the guard for schedules with many disjoint windows",
            state.edge_index
        );
    }
    let upper_bound = if origin.snapped_edge_id == Some(state.edge_index as u32) {
        label.exit_window.end
    } else {
        label.window.end
    };
    let label_id = labels.len();
    labels.push(label);
    existing.push(label_id);
    heap.push(ReverseTemporalHeapEntry {
        label_id,
        upper_bound,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn edge_entry_windows_for_exit(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    edge_index: usize,
    fraction: f64,
    exit_window: TimeWindow,
    range: TimeWindow,
    segment_cache: &mut HashMap<(usize, u64), Vec<EdgeArrivalSegment>>,
) -> Vec<TimeWindow> {
    let fraction = fraction.clamp(0.0, 1.0);
    let segments = segment_cache
        .entry((edge_index, fraction.to_bits()))
        .or_insert_with(|| {
            build_edge_arrival_segments(topology, metrics, context, edge_index, fraction, range)
        });
    let mut windows = Vec::new();
    for segment in segments {
        match segment {
            EdgeArrivalSegment::Affine { entries, duration } => {
                if let Some(window) = entries.intersect(exit_window.shifted(-*duration)) {
                    windows.push(window);
                }
            }
            EdgeArrivalSegment::Constant { entries, exit, .. } => {
                if exit_window.contains(*exit) {
                    windows.push(*entries);
                }
            }
        }
    }
    merge_time_windows(windows)
}

fn build_edge_arrival_segments(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    edge_index: usize,
    fraction: f64,
    range: TimeWindow,
) -> Vec<EdgeArrivalSegment> {
    let edge = topology.routing_edge(edge_index);
    let Some(base_time_s) = metrics
        .edge_metrics
        .get(edge_index)
        .and_then(|metric| metric.travel_time_s)
    else {
        return Vec::new();
    };
    let scenario = context.scenario_features.get(&edge.source_way_id);
    let imported = edge
        .temporal_rule_id
        .and_then(|rule_id| topology.temporal_rule_sets.get(rule_id as usize));
    if !has_time_varying_rules(imported, scenario) {
        let RuleEvaluation::Available { speed_factor } =
            edge_rule_evaluation(topology, context, edge_index, range.start)
        else {
            return Vec::new();
        };
        let duration = Duration::seconds_f64(base_time_s * fraction / f64::from(speed_factor));
        return vec![EdgeArrivalSegment::Affine {
            entries: range,
            duration,
        }];
    }

    #[derive(Clone, Copy)]
    struct MinuteBucket {
        entries: TimeWindow,
        available: Option<(OffsetDateTime, Duration)>,
    }

    let mut bucket_start = range.start
        - Duration::seconds(i64::from(range.start.second()))
        - Duration::nanoseconds(i64::from(range.start.nanosecond()));
    let mut buckets = Vec::new();
    while bucket_start <= range.end {
        let next_minute = bucket_start + Duration::minutes(1);
        let raw_window = TimeWindow {
            start: bucket_start,
            end: next_minute - Duration::nanoseconds(1),
        };
        if let Some(entries) = raw_window.intersect(range) {
            let available = match edge_rule_evaluation(topology, context, edge_index, bucket_start)
            {
                RuleEvaluation::Available { speed_factor } => Some((
                    bucket_start,
                    Duration::seconds_f64(base_time_s * fraction / f64::from(speed_factor)),
                )),
                RuleEvaluation::Unavailable => None,
            };
            buckets.push(MinuteBucket { entries, available });
        }
        bucket_start = next_minute;
    }

    if !has_time_varying_speed_factor(imported, scenario) {
        let mut segments = Vec::new();
        let mut next_available = None::<(OffsetDateTime, Duration)>;
        for bucket in buckets.into_iter().rev() {
            if let Some((entry, duration)) = bucket.available {
                segments.push(EdgeArrivalSegment::Affine {
                    entries: bucket.entries,
                    duration,
                });
                next_available = Some((entry, duration));
                continue;
            }
            if metrics.temporal.allow_wait
                && metrics.temporal.max_wait_s > 0.0
                && let Some((entry, duration)) = next_available
            {
                let eligible = TimeWindow {
                    start: entry - Duration::seconds_f64(metrics.temporal.max_wait_s),
                    end: bucket.entries.end,
                };
                if let Some(entries) = bucket.entries.intersect(eligible) {
                    segments.push(EdgeArrivalSegment::Constant {
                        entries,
                        actual_entry: entry,
                        exit: entry + duration,
                    });
                }
            }
        }
        return segments;
    }

    #[derive(Clone, Copy)]
    struct WaitOption {
        eligible: TimeWindow,
        actual_entry: OffsetDateTime,
        exit: OffsetDateTime,
    }

    let available_starts = buckets
        .iter()
        .filter_map(|bucket| bucket.available)
        .collect::<Vec<_>>();
    let allow_wait = metrics.temporal.allow_wait
        && metrics.temporal.max_wait_s.is_finite()
        && metrics.temporal.max_wait_s > 0.0;
    let max_wait = Duration::seconds_f64(metrics.temporal.max_wait_s.max(0.0));
    let one_nanosecond = Duration::nanoseconds(1);
    let mut segments = Vec::new();

    for bucket in buckets {
        let wait_options = if allow_wait {
            available_starts
                .iter()
                .filter_map(|&(actual_entry, duration)| {
                    let eligible = TimeWindow {
                        start: (actual_entry - max_wait).max(bucket.entries.start),
                        end: actual_entry.min(bucket.entries.end),
                    };
                    (eligible.start <= eligible.end).then_some(WaitOption {
                        eligible,
                        actual_entry,
                        exit: actual_entry + duration,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Partition the minute wherever an option becomes eligible or the
        // affine immediate traversal crosses a constant waited traversal.
        // Within each resulting interval the earliest-arrival choice is
        // fixed, so the reverse preimage remains exact at nanosecond
        // precision without sampling departure seconds.
        let sentinel = bucket.entries.end + one_nanosecond;
        let mut cuts = vec![bucket.entries.start, sentinel];
        for option in &wait_options {
            cuts.push(option.eligible.start);
            if option.eligible.end < bucket.entries.end {
                cuts.push(option.eligible.end + one_nanosecond);
            }
            if let Some((_, immediate_duration)) = bucket.available {
                let after_tie = option.exit - immediate_duration + one_nanosecond;
                if after_tie > bucket.entries.start && after_tie < sentinel {
                    cuts.push(after_tie);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();

        for pair in cuts.windows(2) {
            let entries = TimeWindow {
                start: pair[0],
                end: pair[1] - one_nanosecond,
            };
            if entries.start > entries.end {
                continue;
            }
            enum Choice {
                Immediate(Duration),
                Wait(WaitOption),
            }
            let mut best = bucket.available.map(|(_, duration)| {
                (
                    entries.start + duration,
                    entries.start,
                    Choice::Immediate(duration),
                )
            });
            for &option in &wait_options {
                if !option.eligible.contains(entries.start) {
                    continue;
                }
                if best.as_ref().is_none_or(|(best_exit, best_entry, _)| {
                    option.exit < *best_exit
                        || (option.exit == *best_exit && option.actual_entry < *best_entry)
                }) {
                    best = Some((option.exit, option.actual_entry, Choice::Wait(option)));
                }
            }
            match best.map(|(_, _, choice)| choice) {
                Some(Choice::Immediate(duration)) => {
                    segments.push(EdgeArrivalSegment::Affine { entries, duration });
                }
                Some(Choice::Wait(option)) => {
                    segments.push(EdgeArrivalSegment::Constant {
                        entries,
                        actual_entry: option.actual_entry,
                        exit: option.exit,
                    });
                }
                None => {}
            }
        }
    }
    segments
}

fn edge_rule_evaluation(
    topology: &TopologyBundle,
    context: &TemporalContext,
    edge_index: usize,
    timestamp: OffsetDateTime,
) -> RuleEvaluation {
    let edge = topology.routing_edge(edge_index);
    let scenario = context.scenario_features.get(&edge.source_way_id);
    if scenario.is_some_and(|scenario| scenario.force_closed)
        || (edge.flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION != 0
            && !scenario.is_some_and(scenario_enables_materialized_direction))
    {
        return RuleEvaluation::Unavailable;
    }
    let imported = edge
        .temporal_rule_id
        .and_then(|rule_id| topology.temporal_rule_sets.get(rule_id as usize));
    evaluate_rules(
        imported,
        scenario,
        edge.source_direction,
        timestamp,
        &context.holidays,
    )
}

fn merge_time_windows(mut windows: Vec<TimeWindow>) -> Vec<TimeWindow> {
    windows.sort_by_key(|window| window.start);
    let mut merged = Vec::<TimeWindow>::new();
    for window in windows {
        if let Some(previous) = merged.last_mut()
            && window.start <= previous.end + Duration::nanoseconds(1)
        {
            previous.end = previous.end.max(window.end);
        } else {
            merged.push(window);
        }
    }
    merged
}

fn temporal_route_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    connectivity: &ConnectivityPolicy,
    snap_max_distance_m: f64,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    departure: OffsetDateTime,
    max_labels_per_state: usize,
    context: &TemporalContext,
) -> Result<Option<TemporalCandidatePath>> {
    let mut best = None;
    for origin in origin_candidates {
        for destination in destination_candidates {
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let Some(earliest_arrival_path) = earliest_arrival_route_for_pair(
                topology,
                metrics,
                routing_graph,
                origin,
                destination,
                departure,
                context,
            ) else {
                continue;
            };
            let Some((generalized_cost, arrival, traversals)) = temporal_route_for_pair(
                topology,
                metrics,
                routing_graph,
                origin,
                destination,
                departure,
                max_labels_per_state,
                context,
                Some(earliest_arrival_path),
            )?
            else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|candidate: &TemporalCandidatePath| {
                    generalized_cost < candidate.generalized_cost
                })
            {
                best = Some(TemporalCandidatePath {
                    origin: origin.clone(),
                    destination: destination.clone(),
                    departure,
                    arrival,
                    generalized_cost,
                    traversals,
                    hop_info,
                });
            }
        }
    }
    Ok(best)
}

#[allow(clippy::too_many_arguments)]
fn earliest_arrival_route_for_pair(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    departure: OffsetDateTime,
    context: &TemporalContext,
) -> Option<(f64, OffsetDateTime, Vec<TemporalTraversal>)> {
    if origin.snapped_edge_id.is_none()
        && destination.snapped_edge_id.is_none()
        && origin.snapped_node_id == destination.snapped_node_id
    {
        return Some((0.0, departure, Vec::new()));
    }
    if let (
        Some(origin_edge),
        Some(origin_fraction),
        Some(destination_edge),
        Some(destination_fraction),
    ) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) && origin_edge == destination_edge
        && origin_fraction <= destination_fraction
    {
        let fraction = destination_fraction - origin_fraction;
        let cost = evaluate_edge_at(
            topology,
            metrics,
            context,
            origin_edge as usize,
            departure,
            fraction,
        )?;
        return Some((
            cost.generalized_cost,
            cost.exit_time,
            vec![TemporalTraversal {
                edge_index: origin_edge as usize,
                fraction,
                turn_time_s: 0.0,
                cost,
            }],
        ));
    }

    let mut labels = Vec::<EarliestArrivalLabel>::new();
    let mut best_by_state = HashMap::<SearchStateKey, usize>::new();
    let mut heap = BinaryHeap::<EarliestArrivalHeapEntry>::new();
    let mut best_target = None::<usize>;
    let seeds = if let (Some(edge_id), Some(fraction)) =
        (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        vec![(edge_id as usize, 1.0 - fraction)]
    } else {
        routing_graph
            .outgoing_edges(origin.snapped_node_id as usize)
            .iter()
            .map(|edge_index| (*edge_index as usize, 1.0))
            .collect()
    };

    for (edge_index, full_fraction) in seeds {
        let target_fraction = destination_fraction_for_edge(topology, destination, edge_index);
        let fraction = target_fraction.unwrap_or(full_fraction);
        let Some(cost) =
            evaluate_edge_at(topology, metrics, context, edge_index, departure, fraction)
        else {
            continue;
        };
        let label_id = labels.len();
        labels.push(EarliestArrivalLabel {
            state: SearchStateKey {
                edge_index,
                automaton_state: routing_graph.automaton.transition(0, edge_index),
            },
            arrival: cost.exit_time,
            generalized_cost: cost.generalized_cost,
            previous: None,
            traversal: TemporalTraversal {
                edge_index,
                fraction,
                turn_time_s: 0.0,
                cost,
            },
        });
        if target_fraction.is_some() {
            if best_target.is_none_or(|target| {
                labels[label_id].arrival < labels[target].arrival
                    || (labels[label_id].arrival == labels[target].arrival
                        && labels[label_id].generalized_cost < labels[target].generalized_cost)
            }) {
                best_target = Some(label_id);
            }
            continue;
        }
        let state = labels[label_id].state;
        if best_by_state.get(&state).is_some_and(|existing| {
            labels[*existing].arrival < labels[label_id].arrival
                || (labels[*existing].arrival == labels[label_id].arrival
                    && labels[*existing].generalized_cost <= labels[label_id].generalized_cost)
        }) {
            continue;
        }
        best_by_state.insert(state, label_id);
        heap.push(EarliestArrivalHeapEntry {
            label_id,
            arrival: labels[label_id].arrival,
        });
    }

    while let Some(entry) = heap.pop() {
        let current = &labels[entry.label_id];
        if best_by_state.get(&current.state) != Some(&entry.label_id) {
            continue;
        }
        if best_target.is_some_and(|target| current.arrival >= labels[target].arrival) {
            break;
        }
        let current_state = current.state;
        let current_arrival = current.arrival;
        let current_generalized_cost = current.generalized_cost;
        for transition_index in routing_graph.transition_range(current_state.edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if routing_graph
                .automaton
                .prohibited_sequence_len(current_state.automaton_state, next_edge)
                .is_some()
            {
                continue;
            }
            let turn_time_s =
                turn_penalty_seconds(topology, metrics, current_state.edge_index, next_edge);
            let requested_entry = current_arrival + Duration::seconds_f64(turn_time_s);
            let target_fraction = destination_fraction_for_edge(topology, destination, next_edge);
            let fraction = target_fraction.unwrap_or(1.0);
            let Some(cost) = evaluate_edge_at(
                topology,
                metrics,
                context,
                next_edge,
                requested_entry,
                fraction,
            ) else {
                continue;
            };
            let label_id = labels.len();
            labels.push(EarliestArrivalLabel {
                state: SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: routing_graph
                        .automaton
                        .transition(current_state.automaton_state, next_edge),
                },
                arrival: cost.exit_time,
                generalized_cost: current_generalized_cost
                    + turn_time_s * metrics.turn_costs.cost_time_weight
                    + cost.generalized_cost,
                previous: Some(entry.label_id),
                traversal: TemporalTraversal {
                    edge_index: next_edge,
                    fraction,
                    turn_time_s,
                    cost,
                },
            });
            if target_fraction.is_some() {
                if best_target.is_none_or(|target| {
                    labels[label_id].arrival < labels[target].arrival
                        || (labels[label_id].arrival == labels[target].arrival
                            && labels[label_id].generalized_cost < labels[target].generalized_cost)
                }) {
                    best_target = Some(label_id);
                }
                continue;
            }
            let state = labels[label_id].state;
            if best_by_state.get(&state).is_some_and(|existing| {
                labels[*existing].arrival < labels[label_id].arrival
                    || (labels[*existing].arrival == labels[label_id].arrival
                        && labels[*existing].generalized_cost <= labels[label_id].generalized_cost)
            }) {
                continue;
            }
            best_by_state.insert(state, label_id);
            heap.push(EarliestArrivalHeapEntry {
                label_id,
                arrival: labels[label_id].arrival,
            });
        }
    }

    let target = best_target?;
    let mut traversals = Vec::new();
    let mut cursor = Some(target);
    while let Some(label_id) = cursor {
        traversals.push(labels[label_id].traversal.clone());
        cursor = labels[label_id].previous;
    }
    traversals.reverse();
    Some((
        labels[target].generalized_cost,
        labels[target].arrival,
        traversals,
    ))
}

fn temporal_route_for_pair(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    departure: OffsetDateTime,
    max_labels_per_state: usize,
    context: &TemporalContext,
    incumbent: Option<(f64, OffsetDateTime, Vec<TemporalTraversal>)>,
) -> Result<Option<(f64, OffsetDateTime, Vec<TemporalTraversal>)>> {
    if origin.snapped_edge_id.is_none()
        && destination.snapped_edge_id.is_none()
        && origin.snapped_node_id == destination.snapped_node_id
    {
        return Ok(Some((0.0, departure, Vec::new())));
    }
    if let (
        Some(origin_edge),
        Some(origin_fraction),
        Some(destination_edge),
        Some(destination_fraction),
    ) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) && origin_edge == destination_edge
        && origin_fraction <= destination_fraction
    {
        let fraction = destination_fraction - origin_fraction;
        let Some(cost) = evaluate_edge_at(
            topology,
            metrics,
            context,
            origin_edge as usize,
            departure,
            fraction,
        ) else {
            return Ok(None);
        };
        return Ok(Some((
            cost.generalized_cost,
            cost.exit_time,
            vec![TemporalTraversal {
                edge_index: origin_edge as usize,
                fraction,
                turn_time_s: 0.0,
                cost,
            }],
        )));
    }

    let mut labels = Vec::<TemporalLabel>::new();
    let mut state_labels = HashMap::<SearchStateKey, Vec<usize>>::new();
    let mut heap = BinaryHeap::new();
    let mut best_target: Option<usize> = None;
    let mut best_cost = incumbent
        .as_ref()
        .map_or(f64::INFINITY, |candidate| candidate.0);

    let seeds = if let (Some(edge_id), Some(fraction)) =
        (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        vec![(edge_id as usize, 1.0 - fraction)]
    } else {
        routing_graph
            .outgoing_edges(origin.snapped_node_id as usize)
            .iter()
            .map(|edge_index| (*edge_index as usize, 1.0))
            .collect()
    };

    for (edge_index, full_fraction) in seeds {
        let target_fraction = destination_fraction_for_edge(topology, destination, edge_index);
        let fraction = target_fraction.unwrap_or(full_fraction);
        let Some(cost) =
            evaluate_edge_at(topology, metrics, context, edge_index, departure, fraction)
        else {
            continue;
        };
        let state = SearchStateKey {
            edge_index,
            automaton_state: routing_graph.automaton.transition(0, edge_index),
        };
        let label = TemporalLabel {
            state,
            arrival: cost.exit_time,
            generalized_cost: cost.generalized_cost,
            previous: None,
            traversal: TemporalTraversal {
                edge_index,
                fraction,
                turn_time_s: 0.0,
                cost,
            },
            active: true,
        };
        let label_id = labels.len();
        labels.push(label);
        if target_fraction.is_some() {
            if labels[label_id].generalized_cost < best_cost {
                best_cost = labels[label_id].generalized_cost;
                best_target = Some(label_id);
            }
        } else if insert_nondominated_label(
            &mut labels,
            &mut state_labels,
            label_id,
            max_labels_per_state,
        )? {
            heap.push(TemporalHeapEntry {
                label_id,
                generalized_cost: labels[label_id].generalized_cost,
            });
        }
    }

    while let Some(entry) = heap.pop() {
        if !labels[entry.label_id].active
            || entry.generalized_cost > labels[entry.label_id].generalized_cost
            || entry.generalized_cost >= best_cost
        {
            continue;
        }
        let current_state = labels[entry.label_id].state;
        let current_arrival = labels[entry.label_id].arrival;
        for transition_index in routing_graph.transition_range(current_state.edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if routing_graph
                .automaton
                .prohibited_sequence_len(current_state.automaton_state, next_edge)
                .is_some()
            {
                continue;
            }
            let turn_time_s =
                turn_penalty_seconds(topology, metrics, current_state.edge_index, next_edge);
            let requested_entry = current_arrival + Duration::seconds_f64(turn_time_s);
            let target_fraction = destination_fraction_for_edge(topology, destination, next_edge);
            let fraction = target_fraction.unwrap_or(1.0);
            let Some(cost) = evaluate_edge_at(
                topology,
                metrics,
                context,
                next_edge,
                requested_entry,
                fraction,
            ) else {
                continue;
            };
            let generalized_cost = labels[entry.label_id].generalized_cost
                + turn_time_s * metrics.turn_costs.cost_time_weight
                + cost.generalized_cost;
            if generalized_cost >= best_cost {
                continue;
            }
            let next_state = SearchStateKey {
                edge_index: next_edge,
                automaton_state: routing_graph
                    .automaton
                    .transition(current_state.automaton_state, next_edge),
            };
            let label_id = labels.len();
            labels.push(TemporalLabel {
                state: next_state,
                arrival: cost.exit_time,
                generalized_cost,
                previous: Some(entry.label_id),
                traversal: TemporalTraversal {
                    edge_index: next_edge,
                    fraction,
                    turn_time_s,
                    cost,
                },
                active: true,
            });
            if target_fraction.is_some() {
                best_cost = generalized_cost;
                best_target = Some(label_id);
                continue;
            }
            if insert_nondominated_label(
                &mut labels,
                &mut state_labels,
                label_id,
                max_labels_per_state,
            )? {
                heap.push(TemporalHeapEntry {
                    label_id,
                    generalized_cost,
                });
            }
        }
    }

    let Some(target) = best_target else {
        return Ok(incumbent);
    };
    let arrival = labels[target].arrival;
    let generalized_cost = labels[target].generalized_cost;
    let mut traversals = Vec::new();
    let mut cursor = Some(target);
    while let Some(label_id) = cursor {
        traversals.push(labels[label_id].traversal.clone());
        cursor = labels[label_id].previous;
    }
    traversals.reverse();
    Ok(Some((generalized_cost, arrival, traversals)))
}

fn destination_fraction_for_edge(
    topology: &TopologyBundle,
    destination: &SnappedPoint,
    edge_index: usize,
) -> Option<f64> {
    if destination.snapped_edge_id == Some(edge_index as u32) {
        return Some(destination.snapped_edge_fraction.unwrap_or(1.0));
    }
    (destination.snapped_edge_id.is_none()
        && topology.routing_edge(edge_index).to.0 == destination.snapped_node_id)
        .then_some(1.0)
}

fn insert_nondominated_label(
    labels: &mut [TemporalLabel],
    state_labels: &mut HashMap<SearchStateKey, Vec<usize>>,
    new_label_id: usize,
    max_labels: usize,
) -> Result<bool> {
    let new_state = labels[new_label_id].state;
    let new_cost = labels[new_label_id].generalized_cost;
    let new_arrival = labels[new_label_id].arrival;
    let existing = state_labels.entry(new_state).or_default();
    existing.retain(|label_id| labels[*label_id].active);
    if existing.iter().any(|label_id| {
        let label = &labels[*label_id];
        label.generalized_cost <= new_cost && label.arrival <= new_arrival
    }) {
        labels[new_label_id].active = false;
        return Ok(false);
    }
    let dominated = existing
        .iter()
        .copied()
        .filter(|label_id| {
            let label = &labels[*label_id];
            new_cost <= label.generalized_cost && new_arrival <= label.arrival
        })
        .collect::<Vec<_>>();
    for label_id in dominated {
        labels[label_id].active = false;
    }
    existing.retain(|label_id| labels[*label_id].active);
    if existing.len() >= max_labels {
        bail!(
            "temporal route search exceeded max_labels_per_state={max_labels} at edge {} (automaton state {}); increase the label guard to preserve exactness",
            new_state.edge_index,
            new_state.automaton_state
        );
    }
    existing.push(new_label_id);
    Ok(true)
}

fn build_temporal_route_result(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &crate::RouteRequest,
    context: &TemporalContext,
    candidate: TemporalCandidatePath,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let edge_indexes = candidate
        .traversals
        .iter()
        .map(|traversal| traversal.edge_index)
        .collect::<Vec<_>>();
    let include_detailed_paths = request_returns_detailed_path(&request.returns);
    let needs_node_path =
        include_detailed_paths || !matches!(request.returns.geometry, ReturnGeometry::None);
    let node_path = if needs_node_path {
        let mut nodes = Vec::with_capacity(edge_indexes.len() + 1);
        if let Some(&first_edge) = edge_indexes.first() {
            nodes.push(topology.routing_edge(first_edge).from.0);
            nodes.extend(
                edge_indexes
                    .iter()
                    .map(|edge_index| topology.routing_edge(*edge_index).to.0),
            );
        } else {
            nodes.push(candidate.origin.snapped_node_id);
        }
        nodes
    } else {
        Vec::new()
    };
    let geometry = match request.returns.geometry {
        ReturnGeometry::None => None,
        _ => Some(build_route_geometry(
            topology,
            &edge_indexes,
            &candidate.origin,
            &candidate.destination,
        )),
    };

    let mut distance_m = 0_u64;
    let mut waiting_time_s = 0.0;
    let mut travel_time_s = 0.0;
    let mut component_totals = metrics
        .components
        .iter()
        .map(|component| (component.name.clone(), 0.0))
        .collect::<BTreeMap<_, _>>();
    for traversal in &candidate.traversals {
        distance_m += (f64::from(topology.routing_edge(traversal.edge_index).length_m)
            * traversal.fraction)
            .round() as u64;
        waiting_time_s += traversal.cost.wait_s;
        travel_time_s +=
            traversal.turn_time_s + traversal.cost.wait_s + traversal.cost.travel_time_s;
        for (name, value) in &traversal.cost.components {
            *component_totals.entry(name.clone()).or_default() += value;
        }
    }

    let segments = if request.returns.segment_rows {
        let names = edge_names.unwrap_or(&topology.names);
        Some(
            candidate
                .traversals
                .iter()
                .map(|traversal| {
                    let edge = topology.edge(traversal.edge_index);
                    RouteSegment {
                        edge_id: edge.edge_id.0,
                        from_node_id: edge.from.0,
                        to_node_id: edge.to.0,
                        source_way_id: edge.source_way_id,
                        length_m: (f64::from(edge.length_m) * traversal.fraction).round() as u32,
                        travel_time_s: traversal.cost.travel_time_s + traversal.cost.wait_s,
                        generalized_cost: traversal.cost.generalized_cost,
                        components: traversal.cost.components.clone(),
                        waiting_time_s: traversal.cost.wait_s,
                        entry_time: Some(format_datetime(traversal.cost.entry_time)),
                        exit_time: Some(format_datetime(traversal.cost.exit_time)),
                        road_class: edge.road_class,
                        surface: edge.surface,
                        name: edge
                            .name_index
                            .and_then(|index| names.get(index as usize))
                            .cloned(),
                        violation_type: None,
                    }
                })
                .collect(),
        )
    } else {
        None
    };
    let mut warnings = Vec::new();
    warnings.push(
        "Time-dependent route used the exact multi-label edge search; static CCH acceleration was intentionally bypassed."
            .to_string(),
    );
    if request.alternatives != AlternativeRouteOptions::default() {
        warnings.push(
            "Alternative route enumeration is not applied to time-dependent requests yet."
                .to_string(),
        );
    }
    Ok(RouteResult {
        route_id: request.route_id.clone(),
        origin: candidate.origin,
        destination: candidate.destination,
        outcome: AnalysisOutcome::Legal,
        fallback_used: candidate.hop_info.fallback_used,
        origin_hop_distance_m: candidate.hop_info.origin_hop_distance_m,
        destination_hop_distance_m: candidate.hop_info.destination_hop_distance_m,
        summary: RouteSummary {
            network_distance_m: distance_m,
            network_travel_time_s: travel_time_s,
            network_generalized_cost: candidate.generalized_cost,
            components: component_totals,
            waiting_time_s,
            departure_time: Some(format_datetime(candidate.departure)),
            arrival_time: Some(format_datetime(candidate.arrival)),
            scenario_id: context.scenario_id.clone(),
            illegal_movement_penalty_s: 0.0,
            illegal_movement_penalty_cost: 0.0,
            violation_count: 0,
            violation_types: Vec::new(),
            total_distance_m: distance_m,
            total_travel_time_s: travel_time_s,
            total_generalized_cost: candidate.generalized_cost,
            segment_count: edge_indexes.len(),
        },
        node_path,
        edge_path: if include_detailed_paths {
            edge_indexes
                .iter()
                .map(|edge_index| topology.routing_edge(*edge_index).edge_id.0)
                .collect()
        } else {
            Vec::new()
        },
        geometry,
        hop_segments: candidate.hop_info.hop_segments,
        segments,
        breakdowns: build_breakdowns(topology, metrics, &edge_indexes, &request.returns),
        violations: Vec::new(),
        diagnostics: candidate.hop_info.diagnostics,
        warnings: {
            warnings.extend(candidate.hop_info.warnings);
            warnings
        },
        alternatives: Vec::new(),
    })
}

fn format_datetime(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("OffsetDateTime always formats as RFC 3339")
}

#[cfg(test)]
mod tests {
    use super::*;
    use netweevil_core::{
        AccessMask, CacheBundleId, CompiledCostComponent, CompiledEdgeMetric,
        CompiledProfileBundle, CompiledTemporalProfile, DirectedEdge,
        EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, EdgeId, MinuteInterval, NodeId, RoadClass,
        SmoothnessClass, SurfaceClass, TopologyBundle, TopologyEdgeLayers, TopologyNode,
        TravelMode,
    };

    #[test]
    fn open_only_waits_across_a_direction_flip_boundary() {
        let timestamp = parse_datetime("2026-07-10T09:59:30+08:00").unwrap();
        let rules = TemporalRuleSet {
            rules: vec![TemporalRule {
                day_mask: EVERY_DAY,
                intervals: vec![MinuteInterval {
                    start_minute: 10 * 60,
                    end_minute: 24 * 60,
                }],
                effect: TemporalEffect::OpenOnly,
            }],
        };
        let evaluate =
            |candidate| evaluate_rules(Some(&rules), None, 1, candidate, &HashSet::new());
        let (entry, _) = next_available_entry(timestamp, 120.0, &evaluate).unwrap();
        assert_eq!(entry.minute(), 0);
        assert_eq!(entry.hour(), 10);
        assert_eq!((entry - timestamp).whole_seconds(), 30);
    }

    #[test]
    fn constant_edges_do_not_scan_the_voluntary_wait_horizon() {
        let timestamp = parse_datetime("2026-07-10T09:59:30+08:00").unwrap();
        let evaluations = std::cell::Cell::new(0_u32);
        let result = best_available_entry(timestamp, true, 86_400.0, 10.0, false, false, &|_| {
            evaluations.set(evaluations.get() + 1);
            RuleEvaluation::Available { speed_factor: 1.0 }
        })
        .expect("constant edge is immediately available");
        assert_eq!(result, (timestamp, 1.0));
        assert_eq!(evaluations.get(), 1);
    }

    #[test]
    fn public_holiday_bit_replaces_weekday_bit() {
        let timestamp = parse_datetime("2026-07-10T12:00:00+08:00").unwrap();
        let rule = TemporalRule {
            day_mask: FRIDAY,
            intervals: vec![],
            effect: TemporalEffect::Closed,
        };
        assert!(rule_active(&rule, timestamp, &HashSet::new()));
        assert!(!rule_active(
            &rule,
            timestamp,
            &HashSet::from([timestamp.date()])
        ));
        let every_day = TemporalRule {
            day_mask: EVERY_DAY,
            intervals: vec![],
            effect: TemporalEffect::OpenOnly,
        };
        assert!(rule_active(
            &every_day,
            timestamp,
            &HashSet::from([timestamp.date()])
        ));
    }

    #[test]
    fn wrapping_rule_uses_the_day_on_which_the_interval_started() {
        let rule = TemporalRule {
            day_mask: FRIDAY,
            intervals: vec![MinuteInterval {
                start_minute: 22 * 60,
                end_minute: 6 * 60,
            }],
            effect: TemporalEffect::OpenOnly,
        };
        let friday_early = parse_datetime("2026-07-10T05:00:00+08:00").unwrap();
        let friday_late = parse_datetime("2026-07-10T23:00:00+08:00").unwrap();
        let saturday_early = parse_datetime("2026-07-11T05:00:00+08:00").unwrap();
        assert!(!rule_active(&rule, friday_early, &HashSet::new()));
        assert!(rule_active(&rule, friday_late, &HashSet::new()));
        assert!(rule_active(&rule, saturday_early, &HashSet::new()));
    }

    #[test]
    fn exact_route_changes_across_direction_reversal_boundary() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 0,
                        end_minute: 10 * 60,
                    }],
                    effect: TemporalEffect::BackwardOnly,
                },
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 10 * 60,
                        end_minute: 24 * 60,
                    }],
                    effect: TemporalEffect::ForwardOnly,
                },
            ],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        topology.edge_layers.routing[0].source_direction = 1;
        let metrics = temporal_test_metrics(false);

        let before = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T09:59:00+08:00"),
        )
        .expect("pre-flip route succeeds");
        let after = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T10:01:00+08:00"),
        )
        .expect("post-flip route succeeds");

        assert_eq!(before.edge_path, vec![2]);
        assert_eq!(before.summary.total_travel_time_s, 100.0);
        assert_eq!(after.edge_path, vec![0, 1]);
        assert_eq!(after.summary.total_travel_time_s, 30.0);
    }

    #[test]
    fn earliest_arrival_incumbent_does_not_replace_a_cheaper_slower_route() {
        let topology = temporal_test_topology();
        let mut metrics = temporal_test_metrics(false);
        metrics.edge_metrics[2].travel_time_s = Some(5.0);
        metrics.edge_metrics[2].generalized_cost = Some(100.0);

        let route = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T10:00:00+08:00"),
        )
        .expect("generalized search improves the earliest-arrival incumbent");
        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.total_generalized_cost, 30.0);
        assert_eq!(route.summary.total_travel_time_s, 30.0);
    }

    #[test]
    fn exact_route_waits_for_an_open_only_edge_when_fifo_waiting_is_enabled() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![TemporalRule {
                day_mask: EVERY_DAY,
                intervals: vec![MinuteInterval {
                    start_minute: 10 * 60,
                    end_minute: 24 * 60,
                }],
                effect: TemporalEffect::OpenOnly,
            }],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        let metrics = temporal_test_metrics(true);
        let route = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T09:59:30+08:00"),
        )
        .expect("waiting route succeeds");

        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.waiting_time_s, 30.0);
        assert_eq!(route.summary.total_travel_time_s, 60.0);
        assert_eq!(
            route.summary.arrival_time.as_deref(),
            Some("2026-07-10T10:00:30+08:00")
        );
    }

    #[test]
    fn exact_route_voluntarily_waits_for_a_faster_speed_factor_regime() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 0,
                        end_minute: 10 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(0.1),
                },
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 10 * 60,
                        end_minute: 24 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(10.0),
                },
            ],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        let mut metrics = temporal_test_metrics(true);
        metrics.temporal.max_wait_s = 120.0;
        let route = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T09:59:50+08:00"),
        )
        .expect("waiting for the faster regime produces the earliest route");

        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.waiting_time_s, 10.0);
        assert_eq!(route.summary.total_travel_time_s, 31.0);
        assert_eq!(
            route.summary.arrival_time.as_deref(),
            Some("2026-07-10T10:00:21+08:00")
        );
    }

    #[test]
    fn speed_factor_rules_without_a_wait_policy_are_rejected_before_search() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![TemporalRule {
                day_mask: EVERY_DAY,
                intervals: vec![MinuteInterval {
                    start_minute: 10 * 60,
                    end_minute: 24 * 60,
                }],
                effect: TemporalEffect::SpeedFactor(2.0),
            }],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        let metrics = temporal_test_metrics(false);
        let error = crate::execute_route(
            &topology,
            &metrics,
            &temporal_test_request("2026-07-10T09:59:50+08:00"),
        )
        .expect_err("non-FIFO speed changes require an explicit wait policy");
        assert!(
            error
                .to_string()
                .contains("SpeedFactor rules on source feature 1 require temporal.allow_wait=true")
        );
    }

    #[test]
    fn speed_factor_waiting_rejects_an_overlay_component_with_nondominated_choices() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 0,
                        end_minute: 10 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(0.1),
                },
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 10 * 60,
                        end_minute: 24 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(10.0),
                },
            ],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        let mut metrics = temporal_test_metrics(true);
        metrics.components = vec![CompiledCostComponent {
            name: "exposure".to_string(),
            weight: 1.0,
            edge_values: vec![200.0, 0.0, 0.0],
            scales_with_travel_time: false,
            overlay_name: Some("exposure_factor".to_string()),
            invert_overlay: false,
        }];
        let boundary = parse_datetime("2026-07-10T10:00:00+08:00").unwrap();
        let context = TemporalContext::new(
            None,
            HashSet::new(),
            vec![TemporalOverlaySeries {
                by_feature: HashMap::from([(
                    1,
                    vec![TemporalOverlayInterval {
                        start_unix_s: boundary.unix_timestamp(),
                        end_unix_s: (boundary + Duration::hours(1)).unix_timestamp(),
                        values: BTreeMap::from([("exposure_factor".to_string(), 1.0)]),
                    }],
                )]),
            }],
        );

        let requested = parse_datetime("2026-07-10T09:59:50+08:00").unwrap();
        let waited = evaluate_edge_at(&topology, &metrics, &context, 0, requested, 1.0)
            .expect("voluntary wait option exists");
        let mut immediate_metrics = metrics.clone();
        immediate_metrics.temporal.allow_wait = false;
        let immediate =
            evaluate_edge_at(&topology, &immediate_metrics, &context, 0, requested, 1.0)
                .expect("immediate slow traversal exists");
        assert!(waited.exit_time < immediate.exit_time);
        assert!(waited.generalized_cost > immediate.generalized_cost);
        assert!(waited.components["exposure"] > immediate.components["exposure"]);

        let error = validate_speed_factor_wait_policy(&topology, &metrics, &context)
            .expect_err("the early-arrival and low-exposure choices are nondominated");
        assert!(error.to_string().contains(
            "overlay-backed component 'exposure' is entry-time dependent on that feature"
        ));

        let unrelated_context = TemporalContext::new(
            None,
            HashSet::new(),
            vec![TemporalOverlaySeries {
                by_feature: HashMap::from([(
                    999,
                    vec![TemporalOverlayInterval {
                        start_unix_s: boundary.unix_timestamp(),
                        end_unix_s: (boundary + Duration::hours(1)).unix_timestamp(),
                        values: BTreeMap::from([("exposure_factor".to_string(), 1.0)]),
                    }],
                )]),
            }],
        );
        validate_speed_factor_wait_policy(&topology, &metrics, &unrelated_context)
            .expect("an overlay on an unrelated feature does not affect speed-edge exactness");

        let mut travel_scaled_metrics = temporal_test_metrics(true);
        travel_scaled_metrics.components = vec![CompiledCostComponent {
            name: "uncovered_time".to_string(),
            weight: 1.0,
            edge_values: vec![200.0, 0.0, 0.0],
            scales_with_travel_time: true,
            overlay_name: None,
            invert_overlay: false,
        }];
        validate_speed_factor_wait_policy(
            &topology,
            &travel_scaled_metrics,
            &TemporalContext::default(),
        )
        .expect("a faster earliest-exit choice also dominates a plain travel-scaled component");
    }

    #[test]
    fn reverse_segments_match_forward_voluntary_speed_factor_waiting() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 0,
                        end_minute: 10 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(0.1),
                },
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 10 * 60,
                        end_minute: 24 * 60,
                    }],
                    effect: TemporalEffect::SpeedFactor(10.0),
                },
            ],
        }];
        topology.edge_layers.routing[0].temporal_rule_id = Some(0);
        let mut metrics = temporal_test_metrics(true);
        metrics.temporal.max_wait_s = 120.0;
        let context = TemporalContext::default();
        let range = TimeWindow {
            start: parse_datetime("2026-07-10T09:58:00+08:00").unwrap(),
            end: parse_datetime("2026-07-10T10:01:00+08:00").unwrap(),
        };
        let segments = build_edge_arrival_segments(&topology, &metrics, &context, 0, 1.0, range);

        let mut requested = range.start;
        while requested <= range.end {
            let forward = evaluate_edge_at(&topology, &metrics, &context, 0, requested, 1.0)
                .expect("edge is traversable");
            let segment = segments
                .iter()
                .find(|segment| match segment {
                    EdgeArrivalSegment::Affine { entries, .. }
                    | EdgeArrivalSegment::Constant { entries, .. } => entries.contains(requested),
                })
                .expect("reverse model covers every forward entry");
            let (actual_entry, exit) = match segment {
                EdgeArrivalSegment::Affine { duration, .. } => (requested, requested + *duration),
                EdgeArrivalSegment::Constant {
                    actual_entry, exit, ..
                } => (*actual_entry, *exit),
            };
            assert_eq!(actual_entry, forward.entry_time);
            assert_eq!(exit, forward.exit_time);
            requested += Duration::seconds(1);
        }
    }

    #[test]
    fn arrive_by_finds_a_late_feasible_departure_when_lookback_start_is_closed() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![TemporalRule {
                day_mask: EVERY_DAY,
                intervals: vec![MinuteInterval {
                    start_minute: 10 * 60,
                    end_minute: 24 * 60,
                }],
                effect: TemporalEffect::OpenOnly,
            }],
        }];
        for edge in &mut topology.edge_layers.routing {
            edge.temporal_rule_id = Some(0);
        }
        let metrics = temporal_test_metrics(false);
        let mut request = temporal_test_request("2026-07-10T10:00:00+08:00");
        request.temporal.departure_time = None;
        request.temporal.arrive_by = Some("2026-07-10T10:01:00+08:00".to_string());
        request.temporal.arrive_by_lookback_s = 3_600.0;

        let route = crate::execute_route(&topology, &metrics, &request)
            .expect("a departure after the closed lookback probe must be found");
        let departure = parse_datetime(route.summary.departure_time.as_deref().unwrap()).unwrap();
        let arrival = parse_datetime(route.summary.arrival_time.as_deref().unwrap()).unwrap();
        assert!(departure >= parse_datetime("2026-07-10T10:00:00+08:00").unwrap());
        assert!(arrival <= parse_datetime("2026-07-10T10:01:00+08:00").unwrap());
        assert_eq!(route.edge_path, vec![0, 1]);
    }

    #[test]
    fn arrive_by_does_not_skip_a_narrow_later_opening_window() {
        let mut topology = temporal_test_topology();
        topology.temporal_rule_sets = vec![TemporalRuleSet {
            rules: vec![
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 9 * 60,
                        end_minute: 10 * 60,
                    }],
                    effect: TemporalEffect::OpenOnly,
                },
                TemporalRule {
                    day_mask: EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 11 * 60,
                        end_minute: 11 * 60 + 1,
                    }],
                    effect: TemporalEffect::OpenOnly,
                },
            ],
        }];
        for edge in &mut topology.edge_layers.routing {
            edge.temporal_rule_id = Some(0);
        }
        let metrics = temporal_test_metrics(false);
        let mut request = temporal_test_request("2026-07-10T11:00:00+08:00");
        request.temporal.departure_time = None;
        request.temporal.arrive_by = Some("2026-07-10T12:00:00+08:00".to_string());
        request.temporal.arrive_by_lookback_s = 4.0 * 3_600.0;

        let route = crate::execute_route(&topology, &metrics, &request)
            .expect("later one-minute opening must not be skipped");
        let departure = parse_datetime(route.summary.departure_time.as_deref().unwrap()).unwrap();
        assert!(departure >= parse_datetime("2026-07-10T11:00:00+08:00").unwrap());
        assert!(departure < parse_datetime("2026-07-10T11:01:00+08:00").unwrap());
    }

    #[test]
    fn arrive_by_breaks_equal_latest_departures_by_generalized_cost() {
        let mut topology = temporal_test_topology();
        let mut cheaper_parallel_edge = topology.edge(1);
        cheaper_parallel_edge.edge_id = EdgeId(3);
        cheaper_parallel_edge.source_way_id = 4;
        topology.push_edge(cheaper_parallel_edge);
        topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);

        let mut metrics = temporal_test_metrics(false);
        // Exclude the unrelated direct edge. Both shared-prefix paths take 30
        // seconds, but the edge inserted first has a much higher scalar cost.
        metrics.edge_metrics[1].generalized_cost = Some(200.0);
        metrics.edge_metrics[2].travel_time_s = None;
        metrics.edge_metrics[2].generalized_cost = None;
        metrics.edge_metrics.push(CompiledEdgeMetric {
            edge_id: EdgeId(3),
            travel_time_s: Some(20.0),
            generalized_cost: Some(20.0),
        });

        let mut request = temporal_test_request("2026-07-10T11:00:00+08:00");
        request.temporal.departure_time = None;
        request.temporal.arrive_by = Some("2026-07-10T12:00:00+08:00".to_string());
        request.temporal.arrive_by_lookback_s = 3_600.0;

        let route = crate::execute_route(&topology, &metrics, &request)
            .expect("equal latest departures retain the cheaper suffix");
        assert_eq!(route.edge_path, vec![0, 3]);
        assert_eq!(route.summary.total_generalized_cost, 30.0);
        assert_eq!(
            route.summary.departure_time.as_deref(),
            Some("2026-07-10T11:59:30+08:00")
        );
    }

    #[test]
    fn temporal_scenario_can_enable_a_materialized_direction_hidden_from_static_routing() {
        let mut topology = temporal_test_topology();
        topology.push_edge(DirectedEdge {
            edge_id: EdgeId(3),
            from: NodeId(1),
            to: NodeId(0),
            source_way_id: 1,
            length_m: 100,
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: netweevil_core::NO_FEATURE_ROW,
            source_direction: -1,
            temporal_rule_id: None,
            duration_s: None,
            road_class: RoadClass::Path,
            surface: SurfaceClass::Paved,
            smoothness: SmoothnessClass::Good,
            access_mask: AccessMask::new(AccessMask::FOOT),
            is_toll: false,
            max_speed_kph: None,
            lanes: None,
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
        });
        topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
        let mut metrics = temporal_test_metrics(false);
        metrics.edge_metrics.push(CompiledEdgeMetric {
            edge_id: EdgeId(3),
            travel_time_s: Some(10.0),
            generalized_cost: Some(10.0),
        });
        let mut request = temporal_test_request("2026-07-10T12:00:00+08:00");
        request.origin = crate::LabeledPoint {
            id: "upper".to_string(),
            lon: 6.001,
            lat: 53.0,
            z: None,
        };
        request.destination = crate::LabeledPoint {
            id: "lower".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        };

        let mut static_request = request.clone();
        static_request.temporal = Default::default();
        assert!(crate::execute_route(&topology, &metrics, &static_request).is_err());

        let static_graph = crate::build_routing_graph(&topology, &metrics).unwrap();
        assert!(!static_graph.edge_costs[3].is_finite());
        let context = TemporalContext::new(
            Some(ScenarioOverlay {
                id: Some("reverse".to_string()),
                features: vec![ScenarioFeatureOverride {
                    source_feature_id: 1,
                    force_closed: false,
                    force_open: false,
                    replace_rules: true,
                    rules: vec![TemporalRule {
                        day_mask: EVERY_DAY,
                        intervals: vec![],
                        effect: TemporalEffect::BackwardOnly,
                    }],
                    speed_factor: None,
                }],
            }),
            HashSet::new(),
            Vec::new(),
        );
        let route = execute_temporal_route_with_graph(
            &topology,
            &metrics,
            &static_graph,
            &request,
            &context,
            None,
        )
        .expect("scenario direction rule enables materialized reverse edge");
        assert_eq!(route.edge_path, vec![3]);
    }

    #[test]
    fn temporal_overlay_updates_component_vector_and_scalar_cost() {
        let topology = temporal_test_topology();
        let mut metrics = temporal_test_metrics(false);
        metrics.edge_metrics[0].generalized_cost = Some(30.0);
        metrics.components = vec![CompiledCostComponent {
            name: "sun_time".to_string(),
            weight: 2.0,
            edge_values: vec![10.0, 20.0, 100.0],
            scales_with_travel_time: true,
            overlay_name: Some("shade_fraction".to_string()),
            invert_overlay: true,
        }];
        let timestamp = parse_datetime("2026-07-10T12:00:00+08:00").unwrap();
        let context = TemporalContext::new(
            None,
            HashSet::new(),
            vec![TemporalOverlaySeries {
                by_feature: HashMap::from([(
                    1,
                    vec![TemporalOverlayInterval {
                        start_unix_s: timestamp.unix_timestamp() - 60,
                        end_unix_s: timestamp.unix_timestamp() + 60,
                        values: BTreeMap::from([("shade_fraction".to_string(), 0.75)]),
                    }],
                )]),
            }],
        );
        let evaluated = evaluate_edge_at(&topology, &metrics, &context, 0, timestamp, 1.0)
            .expect("edge is available");
        assert_eq!(evaluated.components["sun_time"], 2.5);
        assert_eq!(evaluated.generalized_cost, 15.0);
    }

    fn temporal_test_request(departure_time: &str) -> crate::RouteRequest {
        crate::RouteRequest {
            route_id: "temporal".to_string(),
            origin: crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            destination: crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
            },
            snap: crate::SnapOptions {
                max_distance_m: 20.0,
                ..Default::default()
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: netweevil_profile::ReturnConfig {
                geometry: ReturnGeometry::Full,
                segment_rows: true,
                ..Default::default()
            },
            alternatives: Default::default(),
            temporal: crate::TemporalRequestOptions {
                departure_time: Some(departure_time.to_string()),
                ..Default::default()
            },
        }
    }

    fn temporal_test_metrics(allow_wait: bool) -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 4,
            profile_id: "foot".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Foot,
            turn_costs: Default::default(),
            components: Vec::new(),
            temporal: CompiledTemporalProfile {
                allow_wait,
                max_wait_s: 120.0,
            },
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(100.0),
                    generalized_cost: Some(100.0),
                },
            ],
        }
    }

    fn temporal_test_topology() -> TopologyBundle {
        let nodes = vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
        ];
        let edge = |edge_id, from, to, length_m| DirectedEdge {
            edge_id: EdgeId(edge_id),
            from: NodeId(from),
            to: NodeId(to),
            source_way_id: i64::from(edge_id) + 1,
            length_m,
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: netweevil_core::NO_FEATURE_ROW,
            source_direction: 1,
            temporal_rule_id: None,
            duration_s: None,
            road_class: RoadClass::Path,
            surface: SurfaceClass::Paved,
            smoothness: SmoothnessClass::Good,
            access_mask: AccessMask::new(AccessMask::FOOT),
            is_toll: false,
            max_speed_kph: None,
            lanes: None,
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        };
        let mut topology = TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes,
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[
                edge(0, 0, 1, 100),
                edge(1, 1, 2, 100),
                edge(2, 0, 2, 300),
            ]),
            turn_restrictions: Vec::new(),
            names: Vec::new(),
            edge_based_topology: Default::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0],
            edge_component_ids: vec![0, 0, 0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
        };
        topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
        topology
    }
}
