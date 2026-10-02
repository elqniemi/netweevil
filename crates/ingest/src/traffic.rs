//! Historical traffic speed import.
//!
//! A source document (YAML or JSON) declares the time zone, the bucket
//! width, weekly speed patterns and their assignment to directed source
//! features. Large inputs keep patterns and assignments in CSV files next to
//! the document. Identical speed vectors are interned, so per-segment inputs
//! that repeat a few shapes store each shape once.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use netweevil_core::{
    AccessMask, RoadClass, TRAFFIC_BACKWARD, TRAFFIC_BUNDLE_SCHEMA_VERSION, TRAFFIC_FORWARD,
    TRAFFIC_SPEED_UNITS_PER_KPH, TopologyBundle, TrafficSpeedBundle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrafficSourceDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_id: Option<String>,
    /// IANA zone of the time-of-week buckets, e.g. `Europe/Amsterdam`.
    pub timezone: String,
    /// Bucket width in minutes; must divide 60.
    pub bucket_minutes: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<TrafficPatternSource>,
    /// CSV with `pattern_id,speeds_kph`, relative to the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns_csv: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<TrafficAssignmentSource>,
    /// CSV with `feature_id,direction` plus either `pattern_id` or
    /// `speeds_kph`, relative to the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments_csv: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrafficPatternSource {
    pub id: String,
    /// One day of buckets (repeated Monday through Sunday) or a full week
    /// starting Monday 00:00. `0` marks a bucket without data.
    pub speeds_kph: Vec<f64>,
    /// Optional one-day profile for Saturday and Sunday when `speeds_kph`
    /// covers a single day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekend_speeds_kph: Option<Vec<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficSourceDirection {
    Forward,
    Backward,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrafficAssignmentSource {
    #[serde(alias = "way_id")]
    pub feature_id: i64,
    pub direction: TrafficSourceDirection,
    pub pattern: String,
}

/// Result of reading a traffic source: the bundle plus input provenance.
#[derive(Debug, Clone)]
pub struct ImportedTraffic {
    pub bundle: TrafficSpeedBundle,
    pub source_paths: Vec<String>,
    pub source_sha256: String,
}

/// How a traffic bundle binds to one topology.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TrafficBindingStats {
    pub feature_count: usize,
    pub matched_feature_count: usize,
    pub matched_edge_count: usize,
}

pub fn read_traffic_source(path: &Path, traffic_id: Option<&str>) -> Result<ImportedTraffic> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading traffic source {}", path.display()))?;
    let document: TrafficSourceDocument = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing traffic source JSON")?,
        Some("yml" | "yaml") => {
            serde_yaml::from_str(&raw).context("parsing traffic source YAML")?
        }
        other => bail!("unsupported traffic source extension {other:?}; use .yml, .yaml or .json"),
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let mut source_paths = vec![path.display().to_string()];
    let mut read_csv = |relative: &Path| -> Result<String> {
        let csv_path = base.join(relative);
        let text = fs::read_to_string(&csv_path)
            .with_context(|| format!("reading {}", csv_path.display()))?;
        hasher.update(text.as_bytes());
        source_paths.push(csv_path.display().to_string());
        Ok(text)
    };
    let patterns_csv = document
        .patterns_csv
        .as_deref()
        .map(&mut read_csv)
        .transpose()?;
    let assignments_csv = document
        .assignments_csv
        .as_deref()
        .map(&mut read_csv)
        .transpose()?;
    let id = traffic_id
        .map(str::to_string)
        .or_else(|| document.traffic_id.clone())
        .context("traffic source has no traffic_id; pass --name")?;
    let bundle = build_traffic_bundle(
        &id,
        &document,
        patterns_csv.as_deref(),
        assignments_csv.as_deref(),
    )?;
    Ok(ImportedTraffic {
        bundle,
        source_paths,
        source_sha256: hex::encode(hasher.finalize()),
    })
}

/// Builds the interned bundle from a parsed document and optional CSV text.
pub fn build_traffic_bundle(
    traffic_id: &str,
    document: &TrafficSourceDocument,
    patterns_csv: Option<&str>,
    assignments_csv: Option<&str>,
) -> Result<TrafficSpeedBundle> {
    if traffic_id.is_empty()
        || !traffic_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("traffic id '{traffic_id}' must be non-empty ASCII letters, digits, '_' or '-'");
    }
    document
        .timezone
        .parse::<chrono_tz::Tz>()
        .map_err(|error| anyhow::anyhow!("invalid timezone '{}': {error}", document.timezone))?;
    let bucket_minutes = document.bucket_minutes;
    if bucket_minutes == 0 || 60 % bucket_minutes != 0 {
        bail!("bucket_minutes must divide 60 (5, 10, 15, 20, 30 or 60), got {bucket_minutes}");
    }
    let per_day = (24 * 60 / bucket_minutes) as usize;
    let mut interner = PatternInterner::new(per_day * 7);

    let mut named = HashMap::<String, u32>::new();
    let mut add_named = |id: &str, speeds: Vec<u16>, interner: &mut PatternInterner| {
        if named
            .insert(id.to_string(), interner.intern(speeds))
            .is_some()
        {
            bail!("pattern '{id}' is defined more than once");
        }
        Ok(())
    };
    for pattern in &document.patterns {
        let week = weekly_speeds(
            &pattern.speeds_kph,
            pattern.weekend_speeds_kph.as_deref(),
            per_day,
        )
        .with_context(|| format!("pattern '{}'", pattern.id))?;
        add_named(&pattern.id, week, &mut interner)?;
    }
    if let Some(text) = patterns_csv {
        let mut reader = csv::Reader::from_reader(text.as_bytes());
        let headers = reader.headers()?.clone();
        let id_column = column(&headers, "pattern_id")?;
        let speeds_column = column(&headers, "speeds_kph")?;
        for (row, record) in reader.records().enumerate() {
            let record = record?;
            let id = &record[id_column];
            let week = parse_speed_list(&record[speeds_column])
                .and_then(|speeds| weekly_speeds(&speeds, None, per_day))
                .with_context(|| format!("patterns_csv row {}", row + 2))?;
            add_named(id, week, &mut interner)?;
        }
    }

    let mut assignments = BTreeMap::<(i64, i8), u32>::new();
    let mut assign = |feature_id: i64, direction: TrafficSourceDirection, pattern: u32| {
        let directions: &[i8] = match direction {
            TrafficSourceDirection::Forward => &[TRAFFIC_FORWARD],
            TrafficSourceDirection::Backward => &[TRAFFIC_BACKWARD],
            TrafficSourceDirection::Both => &[TRAFFIC_BACKWARD, TRAFFIC_FORWARD],
        };
        for &direction in directions {
            if assignments
                .insert((feature_id, direction), pattern)
                .is_some()
            {
                bail!(
                    "feature {feature_id} has more than one assignment for direction {direction}"
                );
            }
        }
        Ok(())
    };
    let pattern_index = |name: &str| {
        named
            .get(name)
            .copied()
            .with_context(|| format!("assignment references unknown pattern '{name}'"))
    };
    for assignment in &document.assignments {
        assign(
            assignment.feature_id,
            assignment.direction,
            pattern_index(&assignment.pattern)?,
        )?;
    }
    if let Some(text) = assignments_csv {
        let mut reader = csv::Reader::from_reader(text.as_bytes());
        let headers = reader.headers()?.clone();
        let feature_column =
            column(&headers, "feature_id").or_else(|_| column(&headers, "way_id"))?;
        let direction_column = column(&headers, "direction")?;
        let pattern_column = column(&headers, "pattern_id").ok();
        let speeds_column = column(&headers, "speeds_kph").ok();
        if pattern_column.is_some() == speeds_column.is_some() {
            bail!("assignments_csv needs exactly one of the columns pattern_id or speeds_kph");
        }
        for (row, record) in reader.records().enumerate() {
            let record = record?;
            let context = || format!("assignments_csv row {}", row + 2);
            let feature_id = record[feature_column]
                .trim()
                .parse::<i64>()
                .with_context(context)?;
            let direction = parse_direction(&record[direction_column]).with_context(context)?;
            let pattern = match (pattern_column, speeds_column) {
                (Some(column), _) => pattern_index(record[column].trim()).with_context(context)?,
                (_, Some(column)) => interner.intern(
                    parse_speed_list(&record[column])
                        .and_then(|speeds| weekly_speeds(&speeds, None, per_day))
                        .with_context(context)?,
                ),
                _ => unreachable!("one column checked above"),
            };
            assign(feature_id, direction, pattern).with_context(context)?;
        }
    }
    if assignments.is_empty() {
        bail!("traffic source assigns no features");
    }

    // Keep only referenced patterns, renumbered in first-use order.
    let mut remap = vec![u32::MAX; interner.patterns.len()];
    let mut pattern_speeds = Vec::new();
    let mut feature_ids = Vec::with_capacity(assignments.len());
    let mut directions = Vec::with_capacity(assignments.len());
    let mut feature_patterns = Vec::with_capacity(assignments.len());
    for ((feature_id, direction), pattern) in assignments {
        let slot = &mut remap[pattern as usize];
        if *slot == u32::MAX {
            *slot = (pattern_speeds.len() / (per_day * 7)) as u32;
            pattern_speeds.extend_from_slice(&interner.patterns[pattern as usize]);
        }
        feature_ids.push(feature_id);
        directions.push(direction);
        feature_patterns.push(*slot);
    }
    let bundle = TrafficSpeedBundle {
        schema_version: TRAFFIC_BUNDLE_SCHEMA_VERSION,
        traffic_id: traffic_id.to_string(),
        timezone: document.timezone.clone(),
        bucket_minutes,
        pattern_speeds,
        feature_ids,
        directions,
        feature_patterns,
    };
    bundle.validate().map_err(anyhow::Error::msg)?;
    Ok(bundle)
}

/// Counts the assignments that match edges of `topology`.
pub fn traffic_binding_stats(
    bundle: &TrafficSpeedBundle,
    topology: &TopologyBundle,
) -> TrafficBindingStats {
    let mut matched = vec![false; bundle.feature_ids.len()];
    let mut matched_edge_count = 0;
    for edge_index in 0..topology.edge_count() {
        let edge = topology.routing_edge(edge_index);
        let direction = if edge.source_direction < 0 {
            TRAFFIC_BACKWARD
        } else {
            TRAFFIC_FORWARD
        };
        let start = bundle
            .feature_ids
            .partition_point(|id| *id < edge.source_way_id);
        if let Some(index) = (start..bundle.feature_ids.len())
            .take_while(|&index| bundle.feature_ids[index] == edge.source_way_id)
            .find(|&index| bundle.directions[index] == direction)
        {
            matched[index] = true;
            matched_edge_count += 1;
        }
    }
    TrafficBindingStats {
        feature_count: bundle.feature_ids.len(),
        matched_feature_count: matched.iter().filter(|matched| **matched).count(),
        matched_edge_count,
    }
}

struct PatternInterner {
    width: usize,
    patterns: Vec<Vec<u16>>,
    lookup: HashMap<Vec<u16>, u32>,
}

impl PatternInterner {
    fn new(width: usize) -> Self {
        Self {
            width,
            patterns: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    fn intern(&mut self, speeds: Vec<u16>) -> u32 {
        debug_assert_eq!(speeds.len(), self.width);
        if let Some(index) = self.lookup.get(&speeds) {
            return *index;
        }
        let index = self.patterns.len() as u32;
        self.lookup.insert(speeds.clone(), index);
        self.patterns.push(speeds);
        index
    }
}

fn weekly_speeds(speeds: &[f64], weekend: Option<&[f64]>, per_day: usize) -> Result<Vec<u16>> {
    let encode = |speeds: &[f64]| -> Result<Vec<u16>> {
        speeds
            .iter()
            .map(|&kph| {
                if !kph.is_finite() || !(0.0..=1000.0).contains(&kph) {
                    bail!("speed {kph} km/h must be finite and within 0..=1000 (0 means no data)");
                }
                let units = (kph * TRAFFIC_SPEED_UNITS_PER_KPH).round() as u16;
                // A positive speed never rounds to the no-data marker.
                Ok(if kph > 0.0 { units.max(1) } else { 0 })
            })
            .collect()
    };
    let week = if speeds.len() == per_day {
        let weekday = encode(speeds)?;
        let weekend = match weekend {
            Some(weekend) if weekend.len() == per_day => encode(weekend)?,
            Some(weekend) => bail!(
                "weekend_speeds_kph has {} values; expected {per_day}",
                weekend.len()
            ),
            None => weekday.clone(),
        };
        let mut week = Vec::with_capacity(per_day * 7);
        for day in 0..7 {
            week.extend_from_slice(if day < 5 { &weekday } else { &weekend });
        }
        week
    } else if speeds.len() == per_day * 7 && weekend.is_none() {
        encode(speeds)?
    } else {
        bail!(
            "speeds_kph has {} values; expected {per_day} (one day) or {} (one week)",
            speeds.len(),
            per_day * 7
        );
    };
    Ok(week)
}

fn parse_speed_list(text: &str) -> Result<Vec<f64>> {
    text.split(|c: char| c == ';' || c.is_whitespace())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<f64>()
                .with_context(|| format!("invalid speed '{value}'"))
        })
        .collect()
}

fn parse_direction(text: &str) -> Result<TrafficSourceDirection> {
    Ok(match text.trim() {
        "forward" | "1" => TrafficSourceDirection::Forward,
        "backward" | "-1" => TrafficSourceDirection::Backward,
        "both" | "0" => TrafficSourceDirection::Both,
        other => bail!("direction must be forward, backward or both, got '{other}'"),
    })
}

fn column(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|header| header.trim() == name)
        .with_context(|| format!("CSV is missing the '{name}' column"))
}

/// Options for [`synthesize_traffic_source`].
#[derive(Debug, Clone)]
pub struct TrafficSynthesisOptions {
    pub timezone: String,
    pub bucket_minutes: u16,
    pub road_classes: Vec<RoadClass>,
    /// Distinct per-feature speed scales per road class.
    pub variants: u32,
}

/// Builds a synthetic, deterministic weekday/weekend congestion profile set
/// for the car-accessible features of `topology`: free-flow speeds by road
/// class, morning and evening dips on weekdays, a milder weekend, and a
/// per-feature scale. Returns the document (patterns inline) and the
/// assignment CSV text.
pub fn synthesize_traffic_source(
    topology: &TopologyBundle,
    traffic_id: &str,
    options: &TrafficSynthesisOptions,
) -> Result<(TrafficSourceDocument, String)> {
    if options.bucket_minutes == 0 || 60 % options.bucket_minutes != 0 {
        bail!("bucket_minutes must divide 60");
    }
    let per_day = (24 * 60 / u32::from(options.bucket_minutes)) as usize;
    let variants = options.variants.max(1);
    let mut patterns = Vec::new();
    for &class in &options.road_classes {
        let (free_flow_kph, peak_depth) = match class {
            RoadClass::Motorway => (115.0, 0.45),
            RoadClass::Trunk => (90.0, 0.4),
            RoadClass::Primary => (65.0, 0.45),
            RoadClass::Secondary => (55.0, 0.4),
            RoadClass::Tertiary => (45.0, 0.35),
            RoadClass::Residential => (28.0, 0.2),
            RoadClass::Service => (18.0, 0.1),
            _ => (40.0, 0.2),
        };
        for variant in 0..variants {
            let scale = 0.85 + 0.3 * f64::from(variant) / f64::from(variants.max(2) - 1);
            let day = |depth: f64, am: f64, pm: f64| -> Vec<f64> {
                (0..per_day)
                    .map(|bucket| {
                        let hour = (bucket as f64 + 0.5) * f64::from(options.bucket_minutes) / 60.0;
                        let dip =
                            |center: f64, width: f64| (-((hour - center) / width).powi(2)).exp();
                        let night = if !(6.0..22.0).contains(&hour) {
                            1.05
                        } else {
                            1.0
                        };
                        let congestion = depth * (am * dip(8.0, 0.9) + pm * dip(17.25, 1.1));
                        (free_flow_kph * scale * night * (1.0 - congestion)).round()
                    })
                    .collect()
            };
            patterns.push(TrafficPatternSource {
                id: format!("{}_{variant}", road_class_name(class)),
                speeds_kph: day(peak_depth, 1.0, 1.0),
                weekend_speeds_kph: Some(day(peak_depth * 0.5, 0.2, 0.8)),
            });
        }
    }
    let mut csv_text = String::from("feature_id,direction,pattern_id\n");
    let mut seen = std::collections::BTreeSet::new();
    for edge_index in 0..topology.edge_count() {
        let edge = topology.routing_edge(edge_index);
        let profile = topology.edge_profile(edge_index);
        if !profile.access_mask.contains(AccessMask::CAR)
            || !options.road_classes.contains(&profile.road_class)
        {
            continue;
        }
        let direction = if edge.source_direction < 0 {
            "backward"
        } else {
            "forward"
        };
        if !seen.insert((edge.source_way_id, direction)) {
            continue;
        }
        // A deterministic per-feature variant (SplitMix64 finalizer).
        let mut mixed = (edge.source_way_id as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        csv_text.push_str(&format!(
            "{},{direction},{}_{}\n",
            edge.source_way_id,
            road_class_name(profile.road_class),
            mixed % u64::from(variants)
        ));
    }
    Ok((
        TrafficSourceDocument {
            traffic_id: Some(traffic_id.to_string()),
            timezone: options.timezone.clone(),
            bucket_minutes: options.bucket_minutes,
            patterns,
            patterns_csv: None,
            assignments: Vec::new(),
            assignments_csv: None,
        },
        csv_text,
    ))
}

fn road_class_name(class: RoadClass) -> &'static str {
    match class {
        RoadClass::Motorway => "motorway",
        RoadClass::Trunk => "trunk",
        RoadClass::Primary => "primary",
        RoadClass::Secondary => "secondary",
        RoadClass::Tertiary => "tertiary",
        RoadClass::Residential => "residential",
        RoadClass::Service => "service",
        RoadClass::Track => "track",
        RoadClass::Ferry => "ferry",
        RoadClass::Path => "path",
        RoadClass::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests;
