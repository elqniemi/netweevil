//! Overture Maps transportation-theme GeoParquet scanner.
//!
//! Reads segment features (subtype `road`) from one GeoParquet file or a
//! directory of them and produces the same [`ScanOutput`] shape as the OSM
//! scanner, so everything from the topology build onward is shared.
//!
//! Overture models connectivity explicitly: segments only connect at
//! `connectors` (linearly referenced points along the segment geometry).
//! Each segment is therefore split into connector-to-connector chunks; a
//! chunk becomes one [`PendingWay`] with synthetic way/node ids. Linearly
//! scoped rules (`between: [0..1]`) — access restrictions, speed limits,
//! surface, flags, and pre-GA `lanes` — are evaluated per chunk. Segment
//! `prohibited_transitions` are mapped onto the shared turn-restriction
//! candidates once all files are scanned, because they may reference
//! segments from other files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use arrow_array::{Array, RecordBatch, cast::AsArray};
use arrow_schema::DataType;
use netweevil_core::geo::haversine_meters;
use netweevil_core::{AccessMask, HighwayClass, SmoothnessClass, SurfaceClass};
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::import::{DatasetImportProgress, DatasetImportStage, emit_progress};
use crate::restrictions::{RestrictionKind, TurnRestrictionCandidate, ViaSpec};
use crate::scan::{ObjectCounts, PendingWay, ScanOutput};

/// Segment attribute columns consumed by the scanner. Columns absent from a
/// file's schema (e.g. `lanes` after its removal from the GA schema) are
/// simply skipped, so releases with different schemas all import.
const SEGMENT_COLUMNS: &[&str] = &[
    "id",
    "subtype",
    "class",
    "subclass",
    "names",
    "connectors",
    "geometry",
    "access_restrictions",
    "speed_limits",
    "road_surface",
    "road_flags",
    "prohibited_transitions",
    "lanes",
];

const ALL_MODES: u16 = AccessMask::CAR
    | AccessMask::BICYCLE
    | AccessMask::FOOT
    | AccessMask::TRANSIT
    | AccessMask::HGV;
const MOTOR_MODES: u16 = AccessMask::CAR | AccessMask::TRANSIT | AccessMask::HGV;

pub(crate) fn scan_overture(
    source_path: &Path,
    _source_size_bytes: u64,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<ScanOutput> {
    let files = collect_parquet_files(source_path)?;
    if files.is_empty() {
        bail!(
            "no parquet files found in Overture source {}",
            source_path.display()
        );
    }

    let mut state = OvertureState::default();
    for (file_index, file) in files.iter().enumerate() {
        emit_progress(
            progress,
            DatasetImportStage::ScanRoutableObjects,
            Some(file_index as f64 / files.len() as f64 * 100.0),
            format!(
                "Scanning Overture segments {}/{} ({} chunks kept)",
                file_index + 1,
                files.len(),
                state.pending_ways.len()
            ),
        );
        scan_segment_file(file, &mut state)
            .with_context(|| format!("scanning Overture parquet {}", file.display()))?;
    }

    let restriction_candidates = state.resolve_transitions();
    emit_progress(
        progress,
        DatasetImportStage::ScanRoutableObjects,
        Some(100.0),
        format!(
            "Scanning Overture segments 100% ({} chunks kept, {} restrictions)",
            state.pending_ways.len(),
            restriction_candidates.len()
        ),
    );

    Ok(ScanOutput {
        pending_ways: state.pending_ways,
        restriction_candidates,
        node_coords: state.node_coords,
        traffic_signal_nodes: Default::default(),
        counts: state.counts,
    })
}

fn collect_parquet_files(source_path: &Path) -> Result<Vec<PathBuf>> {
    if source_path.is_file() {
        return Ok(vec![source_path.to_path_buf()]);
    }
    let mut files = Vec::new();
    let mut stack = vec![source_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading directory {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("parquet") || ext.eq_ignore_ascii_case("geoparquet")
                })
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

// --- Parquet row decoding ---------------------------------------------------

/// One Overture segment row, deserialized from the arrow JSON encoding of
/// the attribute columns. Every field is optional so schema differences
/// between releases (and null-heavy rows) decode without errors.
#[derive(Debug, Default, Deserialize)]
struct SegmentRow {
    id: Option<String>,
    subtype: Option<String>,
    class: Option<String>,
    subclass: Option<String>,
    names: Option<Names>,
    connectors: Option<Vec<ConnectorRef>>,
    access_restrictions: Option<Vec<AccessRule>>,
    speed_limits: Option<Vec<SpeedLimitRule>>,
    road_surface: Option<Vec<SurfaceRule>>,
    road_flags: Option<Vec<FlagRule>>,
    prohibited_transitions: Option<Vec<TransitionRule>>,
    lanes: Option<Vec<LaneRule>>,
}

#[derive(Debug, Default, Deserialize)]
struct Names {
    primary: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConnectorRef {
    connector_id: Option<String>,
    at: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct When {
    heading: Option<String>,
    during: Option<String>,
    mode: Option<Vec<String>>,
    using: Option<Vec<serde_json::Value>>,
    recognized: Option<Vec<serde_json::Value>>,
    vehicle: Option<Vec<serde_json::Value>>,
}

impl When {
    /// Rules gated on time of day, usage purpose, recognized status, or
    /// vehicle dimensions cannot be represented on a static edge; they are
    /// skipped rather than applied unconditionally.
    fn is_conditional(&self) -> bool {
        self.during.is_some()
            || self.using.as_ref().is_some_and(|list| !list.is_empty())
            || self
                .recognized
                .as_ref()
                .is_some_and(|list| !list.is_empty())
            || self.vehicle.as_ref().is_some_and(|list| !list.is_empty())
    }
}

#[derive(Debug, Default, Deserialize)]
struct AccessRule {
    access_type: Option<String>,
    when: Option<When>,
    between: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Default, Deserialize)]
struct Speed {
    value: Option<f64>,
    unit: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SpeedLimitRule {
    max_speed: Option<Speed>,
    #[allow(dead_code)]
    min_speed: Option<Speed>,
    when: Option<When>,
    between: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Default, Deserialize)]
struct SurfaceRule {
    value: Option<String>,
    between: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Default, Deserialize)]
struct FlagRule {
    values: Option<Vec<String>>,
    between: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Default, Deserialize)]
struct SequenceEntry {
    connector_id: Option<String>,
    segment_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TransitionRule {
    sequence: Option<Vec<SequenceEntry>>,
    final_heading: Option<String>,
    when: Option<When>,
}

/// Pre-GA releases carried a `lanes` column; both observed encodings decode
/// through this shape: a plain lane object (`direction`) or a linearly
/// scoped rule whose `value` is a list of lane objects.
#[derive(Debug, Default, Deserialize)]
struct LaneRule {
    direction: Option<String>,
    value: Option<Vec<LaneRule>>,
    between: Option<Vec<Option<f64>>>,
}

fn between_range(between: &Option<Vec<Option<f64>>>) -> (f64, f64) {
    match between.as_deref() {
        Some([Some(start), Some(end)]) => (start.min(*end), start.max(*end)),
        Some([Some(start), None]) => (*start, 1.0),
        Some([None, Some(end)]) => (0.0, *end),
        _ => (0.0, 1.0),
    }
}

/// Fraction of the chunk `[chunk_start, chunk_end]` covered by the rule.
fn chunk_overlap(rule: (f64, f64), chunk_start: f64, chunk_end: f64) -> f64 {
    let span = (chunk_end - chunk_start).max(f64::EPSILON);
    ((rule.1.min(chunk_end) - rule.0.max(chunk_start)).max(0.0)) / span
}

// --- Scan state --------------------------------------------------------------

#[derive(Debug, Clone)]
struct SegmentTopology {
    /// Node id at each cut point, in order along the segment.
    cut_nodes: Vec<i64>,
    /// Connector id (when the cut point is a connector) at each cut point.
    cut_connectors: Vec<Option<String>>,
    /// Synthetic way id of the chunk between cut `i` and cut `i + 1`.
    chunk_way_ids: Vec<i64>,
}

impl SegmentTopology {
    fn connector_position(&self, connector_id: &str) -> Option<usize> {
        self.cut_connectors
            .iter()
            .position(|cut| cut.as_deref() == Some(connector_id))
    }
}

#[derive(Debug, Default)]
struct RawTransition {
    from_segment_id: String,
    sequence: Vec<(String, String)>,
    final_heading: Option<String>,
    heading: Option<String>,
    mode_mask: AccessMask,
}

#[derive(Debug, Default)]
struct OvertureState {
    pending_ways: Vec<PendingWay>,
    node_coords: FxHashMap<i64, (f64, f64)>,
    counts: ObjectCounts,
    connector_nodes: FxHashMap<String, i64>,
    segment_topologies: FxHashMap<String, SegmentTopology>,
    transitions: Vec<RawTransition>,
    next_node_id: i64,
    next_way_id: i64,
}

impl OvertureState {
    fn allocate_node(&mut self) -> i64 {
        self.next_node_id += 1;
        self.next_node_id
    }

    fn allocate_way(&mut self) -> i64 {
        self.next_way_id += 1;
        self.next_way_id
    }

    fn connector_node(&mut self, connector_id: &str) -> i64 {
        if let Some(&node) = self.connector_nodes.get(connector_id) {
            return node;
        }
        self.next_node_id += 1;
        self.connector_nodes
            .insert(connector_id.to_string(), self.next_node_id);
        self.next_node_id
    }
}

fn scan_segment_file(path: &Path, state: &mut OvertureState) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let root_schema = builder.schema().clone();

    // Segment files must carry geometry and connectors; other theme files
    // (connectors, or non-transportation themes) are skipped.
    if root_schema.column_with_name("connectors").is_none()
        || root_schema.column_with_name("geometry").is_none()
    {
        return Ok(());
    }

    let projected_roots: Vec<usize> = root_schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, field)| SEGMENT_COLUMNS.contains(&field.name().as_str()))
        .map(|(index, _)| index)
        .collect();
    let mask = ProjectionMask::roots(builder.parquet_schema(), projected_roots);
    let reader = builder.with_projection(mask).build()?;

    for batch in reader {
        let batch = batch?;
        scan_segment_batch(&batch, state)?;
    }
    Ok(())
}

fn scan_segment_batch(batch: &RecordBatch, state: &mut OvertureState) -> Result<()> {
    let schema = batch.schema();
    let geometry_index = schema
        .column_with_name("geometry")
        .map(|(index, _)| index)
        .context("segment batch has no geometry column")?;

    // Attribute columns round-trip through arrow's JSON encoding into serde
    // structs; this stays correct across schema releases because unknown
    // fields are ignored and missing ones default to None.
    let attribute_indices: Vec<usize> = (0..batch.num_columns())
        .filter(|&index| index != geometry_index)
        .collect();
    let attribute_batch = batch
        .project(&attribute_indices)
        .context("projecting attribute columns")?;
    let rows = decode_rows(&attribute_batch)?;

    let geometry = batch.column(geometry_index);
    for (row_index, row) in rows.into_iter().enumerate() {
        state.counts.ways += 1;
        let Some(line) = decode_wkb_linestring_at(geometry, row_index)? else {
            continue;
        };
        ingest_segment(row, &line, state);
    }
    Ok(())
}

fn decode_rows(batch: &RecordBatch) -> Result<Vec<SegmentRow>> {
    let mut writer = arrow_json::ArrayWriter::new(Vec::new());
    writer.write(batch).context("encoding segment batch")?;
    writer.finish().context("finishing segment batch")?;
    serde_json::from_slice(&writer.into_inner()).context("decoding segment rows")
}

fn decode_wkb_linestring_at(geometry: &dyn Array, row: usize) -> Result<Option<Vec<(f64, f64)>>> {
    if geometry.is_null(row) {
        return Ok(None);
    }
    let bytes: &[u8] = match geometry.data_type() {
        DataType::Binary => geometry.as_binary::<i32>().value(row),
        DataType::LargeBinary => geometry.as_binary::<i64>().value(row),
        DataType::BinaryView => geometry.as_binary_view().value(row),
        other => bail!("unsupported geometry column type {other}"),
    };
    Ok(parse_wkb_linestring(bytes))
}

/// Minimal WKB decoder for the LineString geometries Overture segments use.
/// Returns None for other geometry types.
fn parse_wkb_linestring(bytes: &[u8]) -> Option<Vec<(f64, f64)>> {
    const WKB_LINESTRING: u32 = 2;
    const EWKB_SRID_FLAG: u32 = 0x2000_0000;
    const WKB_Z_FLAG: u32 = 0x8000_0000;
    const ISO_TYPE_MODULUS: u32 = 1000;

    let little_endian = match bytes.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    };
    let read_f64 = |offset: usize| -> Option<f64> {
        let raw: [u8; 8] = bytes.get(offset..offset + 8)?.try_into().ok()?;
        Some(if little_endian {
            f64::from_le_bytes(raw)
        } else {
            f64::from_be_bytes(raw)
        })
    };

    let raw_type = read_u32(1)?;
    let has_z = raw_type & WKB_Z_FLAG != 0 || (raw_type & !EWKB_SRID_FLAG) / ISO_TYPE_MODULUS == 1;
    let base_type = (raw_type & 0x0FFF_FFFF) % ISO_TYPE_MODULUS;
    if base_type != WKB_LINESTRING {
        return None;
    }
    let mut offset = 5;
    if raw_type & EWKB_SRID_FLAG != 0 {
        offset += 4;
    }
    let point_count = read_u32(offset)? as usize;
    offset += 4;
    let stride = if has_z { 24 } else { 16 };
    let mut points = Vec::with_capacity(point_count);
    for _ in 0..point_count {
        let lon = read_f64(offset)?;
        let lat = read_f64(offset + 8)?;
        points.push((lon, lat));
        offset += stride;
    }
    (points.len() >= 2).then_some(points)
}

// --- Segment mapping ----------------------------------------------------------

fn ingest_segment(row: SegmentRow, line: &[(f64, f64)], state: &mut OvertureState) {
    if row.subtype.as_deref() != Some("road") {
        return;
    }
    if segment_is_unroutable(&row) {
        return;
    }
    let Some(segment_id) = row.id.clone() else {
        return;
    };

    let highway = classify_overture_road(row.class.as_deref(), row.subclass.as_deref());
    let road_class = highway.road_class();
    let base_access = base_access_for_class(row.class.as_deref());
    let name = row.names.as_ref().and_then(|names| names.primary.clone());

    // Cumulative linear position of each geometry vertex.
    let fractions = vertex_fractions(line);

    // Cut the segment at its connectors (plus both endpoints), keeping the
    // connector identity so other segments and transitions join up.
    let mut cut_indices: Vec<(usize, Option<String>)> = vec![(0, None), (line.len() - 1, None)];
    let mut connectors: Vec<ConnectorRef> = row.connectors.unwrap_or_default();
    connectors.sort_by(|a, b| {
        a.at.unwrap_or(0.0)
            .partial_cmp(&b.at.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for connector in connectors {
        let (Some(connector_id), at) = (connector.connector_id, connector.at.unwrap_or(0.0)) else {
            continue;
        };
        let vertex = nearest_vertex(&fractions, at);
        match cut_indices.iter_mut().find(|(index, _)| *index == vertex) {
            Some((_, existing @ None)) => *existing = Some(connector_id),
            Some(_) => {}
            None => cut_indices.push((vertex, Some(connector_id))),
        }
    }
    cut_indices.sort_by_key(|(index, _)| *index);
    cut_indices.dedup_by(|a, b| a.0 == b.0);

    // Assign node ids: connector cuts intern globally, everything else is
    // fresh. Interior (non-cut) vertices always get fresh ids.
    let mut vertex_nodes: FxHashMap<usize, i64> = FxHashMap::default();
    let mut cut_nodes = Vec::with_capacity(cut_indices.len());
    let mut cut_connectors = Vec::with_capacity(cut_indices.len());
    for (vertex, connector_id) in &cut_indices {
        let node = match connector_id {
            Some(connector_id) => state.connector_node(connector_id),
            None => state.allocate_node(),
        };
        vertex_nodes.insert(*vertex, node);
        cut_nodes.push(node);
        cut_connectors.push(connector_id.clone());
    }

    let access_rules = row.access_restrictions.unwrap_or_default();
    let speed_rules = row.speed_limits.unwrap_or_default();
    let surface_rules = row.road_surface.unwrap_or_default();
    let lane_rules = row.lanes.unwrap_or_default();

    let mut chunk_way_ids = Vec::with_capacity(cut_indices.len().saturating_sub(1));
    for cut_pair in cut_indices.windows(2) {
        let (start_vertex, end_vertex) = (cut_pair[0].0, cut_pair[1].0);
        let chunk_start = fractions[start_vertex];
        let chunk_end = fractions[end_vertex];
        let way_id = state.allocate_way();
        chunk_way_ids.push(way_id);

        let mut node_ids = Vec::with_capacity(end_vertex - start_vertex + 1);
        for (vertex, coord) in line
            .iter()
            .enumerate()
            .take(end_vertex + 1)
            .skip(start_vertex)
        {
            let node = match vertex_nodes.get(&vertex) {
                Some(&node) => node,
                None => state.allocate_node(),
            };
            state.node_coords.entry(node).or_insert((coord.0, coord.1));
            state.counts.nodes += 1;
            node_ids.push(node);
        }

        let (forward_access, reverse_access) =
            chunk_access(base_access, &access_rules, chunk_start, chunk_end);
        let forward_max_speed_kph =
            chunk_max_speed(&speed_rules, chunk_start, chunk_end, "forward");
        let reverse_max_speed_kph =
            chunk_max_speed(&speed_rules, chunk_start, chunk_end, "backward");
        let (forward_lanes, reverse_lanes) = chunk_lanes(&lane_rules, chunk_start, chunk_end);

        state.pending_ways.push(PendingWay {
            osm_way_id: way_id,
            node_ids,
            road_class,
            highway,
            duration_s: None,
            surface: chunk_surface(&surface_rules, chunk_start, chunk_end),
            smoothness: SmoothnessClass::Unknown,
            forward_access_mask: forward_access,
            reverse_access_mask: reverse_access,
            is_toll: false,
            is_roundabout: false,
            forward_extra_flags: 0,
            reverse_extra_flags: 0,
            forward_max_speed_kph,
            reverse_max_speed_kph,
            forward_lanes,
            reverse_lanes,
            name: name.clone(),
        });
    }

    for transition in row.prohibited_transitions.unwrap_or_default() {
        state.counts.relations += 1;
        let Some(raw) = raw_transition(&segment_id, transition) else {
            continue;
        };
        state.transitions.push(raw);
    }

    state.segment_topologies.insert(
        segment_id,
        SegmentTopology {
            cut_nodes,
            cut_connectors,
            chunk_way_ids,
        },
    );
}

fn segment_is_unroutable(row: &SegmentRow) -> bool {
    row.road_flags
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|rule| chunk_overlap(between_range(&rule.between), 0.0, 1.0) > 0.5)
        .flat_map(|rule| rule.values.as_deref().unwrap_or_default())
        .any(|flag| flag == "is_under_construction" || flag == "is_abandoned")
}

fn vertex_fractions(line: &[(f64, f64)]) -> Vec<f64> {
    let mut cumulative = Vec::with_capacity(line.len());
    let mut total = 0.0;
    cumulative.push(0.0);
    for pair in line.windows(2) {
        total += haversine_meters(pair[0].0, pair[0].1, pair[1].0, pair[1].1);
        cumulative.push(total);
    }
    if total > 0.0 {
        for value in &mut cumulative {
            *value /= total;
        }
    }
    cumulative
}

fn nearest_vertex(fractions: &[f64], at: f64) -> usize {
    fractions
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - at)
                .abs()
                .partial_cmp(&(*b - at).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn classify_overture_road(class: Option<&str>, subclass: Option<&str>) -> HighwayClass {
    let is_link = subclass == Some("link");
    match class.unwrap_or("unknown") {
        "motorway" if is_link => HighwayClass::MotorwayLink,
        "motorway" => HighwayClass::Motorway,
        "trunk" if is_link => HighwayClass::TrunkLink,
        "trunk" => HighwayClass::Trunk,
        "primary" if is_link => HighwayClass::PrimaryLink,
        "primary" => HighwayClass::Primary,
        "secondary" if is_link => HighwayClass::SecondaryLink,
        "secondary" => HighwayClass::Secondary,
        "tertiary" if is_link => HighwayClass::TertiaryLink,
        "tertiary" => HighwayClass::Tertiary,
        "residential" => HighwayClass::Residential,
        "living_street" => HighwayClass::LivingStreet,
        "unclassified" => HighwayClass::Unclassified,
        "service" => HighwayClass::Service,
        "pedestrian" => HighwayClass::Pedestrian,
        "footway" => HighwayClass::Footway,
        "steps" => HighwayClass::Steps,
        "path" => HighwayClass::Path,
        "track" => HighwayClass::Track,
        "cycleway" => HighwayClass::Cycleway,
        "bridleway" => HighwayClass::Bridleway,
        _ => HighwayClass::Unknown,
    }
}

/// Default mode access before `access_restrictions` are applied; mirrors the
/// per-highway defaults the OSM classifier uses.
fn base_access_for_class(class: Option<&str>) -> u16 {
    match class.unwrap_or("unknown") {
        "motorway" => AccessMask::CAR | AccessMask::HGV,
        "trunk" | "primary" | "secondary" | "tertiary" | "residential" | "living_street"
        | "unclassified" | "service" | "track" => {
            AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT | AccessMask::HGV
        }
        "pedestrian" | "footway" | "steps" | "path" | "bridleway" => AccessMask::FOOT,
        "cycleway" => AccessMask::BICYCLE | AccessMask::FOOT,
        _ => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT,
    }
}

fn travel_mode_bits(mode: &str) -> u16 {
    match mode {
        "vehicle" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT,
        "motor_vehicle" => MOTOR_MODES,
        "car" | "motorcycle" => AccessMask::CAR,
        "truck" | "hgv" => AccessMask::HGV,
        "bus" => AccessMask::TRANSIT,
        "foot" => AccessMask::FOOT,
        "bicycle" => AccessMask::BICYCLE,
        _ => 0,
    }
}

fn when_mode_bits(when: Option<&When>) -> u16 {
    match when.and_then(|when| when.mode.as_deref()) {
        Some(modes) if !modes.is_empty() => modes
            .iter()
            .fold(0, |bits, mode| bits | travel_mode_bits(mode)),
        _ => ALL_MODES,
    }
}

fn chunk_access(
    base_access: u16,
    rules: &[AccessRule],
    chunk_start: f64,
    chunk_end: f64,
) -> (AccessMask, AccessMask) {
    let mut forward = base_access;
    let mut reverse = base_access;
    for rule in rules {
        if chunk_overlap(between_range(&rule.between), chunk_start, chunk_end) <= 0.5 {
            continue;
        }
        if rule.when.as_ref().is_some_and(When::is_conditional) {
            continue;
        }
        let mode_bits = when_mode_bits(rule.when.as_ref());
        let heading = rule.when.as_ref().and_then(|when| when.heading.as_deref());
        let (apply_forward, apply_reverse) = match heading {
            Some("forward") => (true, false),
            Some("backward") => (false, true),
            _ => (true, true),
        };
        match rule.access_type.as_deref() {
            Some("denied") => {
                if apply_forward {
                    forward &= !mode_bits;
                }
                if apply_reverse {
                    reverse &= !mode_bits;
                }
            }
            Some("allowed") | Some("designated") => {
                // An unscoped allow reopens at most the class defaults; a
                // mode-scoped allow grants exactly those modes.
                let granted = if rule
                    .when
                    .as_ref()
                    .and_then(|when| when.mode.as_deref())
                    .is_some_and(|modes| !modes.is_empty())
                {
                    mode_bits
                } else {
                    base_access
                };
                if apply_forward {
                    forward |= granted;
                }
                if apply_reverse {
                    reverse |= granted;
                }
            }
            _ => {}
        }
    }
    (AccessMask::new(forward), AccessMask::new(reverse))
}

fn chunk_max_speed(
    rules: &[SpeedLimitRule],
    chunk_start: f64,
    chunk_end: f64,
    heading: &str,
) -> Option<f32> {
    rules
        .iter()
        .filter(|rule| {
            let when = rule.when.as_ref();
            if when.is_some_and(When::is_conditional) {
                return false;
            }
            if let Some(rule_heading) = when.and_then(|when| when.heading.as_deref())
                && rule_heading != heading
            {
                return false;
            }
            // Mode-scoped limits count when they cover motorized traffic.
            when_mode_bits(when) & MOTOR_MODES != 0
        })
        .map(|rule| {
            (
                chunk_overlap(between_range(&rule.between), chunk_start, chunk_end),
                rule,
            )
        })
        .filter(|(overlap, _)| *overlap > 0.0)
        .max_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .and_then(|(_, rule)| speed_kph(rule.max_speed.as_ref()))
}

fn speed_kph(speed: Option<&Speed>) -> Option<f32> {
    let speed = speed?;
    let value = speed.value? as f32;
    if !(value.is_finite() && value > 0.0) {
        return None;
    }
    match speed.unit.as_deref() {
        Some("mph") => Some(value * 1.609_344),
        _ => Some(value),
    }
}

fn chunk_lanes(rules: &[LaneRule], chunk_start: f64, chunk_end: f64) -> (Option<u8>, Option<u8>) {
    let mut forward = 0_u16;
    let mut reverse = 0_u16;
    let mut seen_any = false;
    let count = |lane: &LaneRule, forward: &mut u16, reverse: &mut u16, seen: &mut bool| match lane
        .direction
        .as_deref()
    {
        Some("forward") => {
            *forward += 1;
            *seen = true;
        }
        Some("backward") => {
            *reverse += 1;
            *seen = true;
        }
        Some("both_ways") | Some("alternating") => {
            *forward += 1;
            *reverse += 1;
            *seen = true;
        }
        _ => {}
    };
    for rule in rules {
        if let Some(scoped_lanes) = &rule.value {
            if chunk_overlap(between_range(&rule.between), chunk_start, chunk_end) <= 0.5 {
                continue;
            }
            for lane in scoped_lanes {
                count(lane, &mut forward, &mut reverse, &mut seen_any);
            }
        } else {
            count(rule, &mut forward, &mut reverse, &mut seen_any);
        }
    }
    if !seen_any {
        return (None, None);
    }
    let clamp = |lanes: u16| (lanes > 0).then(|| lanes.min(u8::MAX as u16) as u8);
    (clamp(forward), clamp(reverse))
}

fn chunk_surface(rules: &[SurfaceRule], chunk_start: f64, chunk_end: f64) -> SurfaceClass {
    rules
        .iter()
        .map(|rule| {
            (
                chunk_overlap(between_range(&rule.between), chunk_start, chunk_end),
                rule,
            )
        })
        .filter(|(overlap, _)| *overlap > 0.0)
        .max_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, rule)| match rule.value.as_deref() {
            Some("paved") | Some("paving_stones") | Some("metal") => SurfaceClass::Paved,
            Some("gravel") => SurfaceClass::Gravel,
            Some("dirt") => SurfaceClass::Dirt,
            Some("unpaved") => SurfaceClass::Ground,
            _ => SurfaceClass::Unknown,
        })
        .unwrap_or(SurfaceClass::Unknown)
}

// --- Prohibited transitions ---------------------------------------------------

fn raw_transition(segment_id: &str, transition: TransitionRule) -> Option<RawTransition> {
    let when = transition.when;
    if when.as_ref().is_some_and(When::is_conditional) {
        return None;
    }
    let sequence: Vec<(String, String)> = transition
        .sequence
        .unwrap_or_default()
        .into_iter()
        .map(|entry| Some((entry.connector_id?, entry.segment_id?)))
        .collect::<Option<_>>()?;
    if sequence.is_empty() {
        return None;
    }
    let mode_bits = match when.as_ref().and_then(|when| when.mode.as_deref()) {
        Some(modes) if !modes.is_empty() => modes
            .iter()
            .fold(0, |bits, mode| bits | travel_mode_bits(mode)),
        _ => AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
    };
    if mode_bits == 0 {
        return None;
    }
    Some(RawTransition {
        from_segment_id: segment_id.to_string(),
        sequence,
        final_heading: transition.final_heading,
        heading: when.and_then(|when| when.heading),
        mode_mask: AccessMask::new(mode_bits),
    })
}

impl OvertureState {
    fn resolve_transitions(&mut self) -> Vec<TurnRestrictionCandidate> {
        let transitions = std::mem::take(&mut self.transitions);
        let mut candidates = Vec::new();
        let mut relation_id = 0_i64;
        for transition in transitions {
            relation_id += 1;
            self.resolve_transition(&transition, relation_id, &mut candidates);
        }
        candidates
    }

    fn resolve_transition(
        &self,
        transition: &RawTransition,
        relation_id: i64,
        candidates: &mut Vec<TurnRestrictionCandidate>,
    ) {
        let Some(from_topology) = self.segment_topologies.get(&transition.from_segment_id) else {
            return;
        };
        let (first_connector, _) = &transition.sequence[0];
        let Some(entry_position) = from_topology.connector_position(first_connector) else {
            return;
        };

        // Chunks that traverse segments between consecutive sequence
        // connectors; the last entry names the destination segment instead.
        let mut via_way_ids = Vec::new();
        for pair in transition.sequence.windows(2) {
            let (enter_connector, via_segment_id) = &pair[0];
            let (exit_connector, _) = &pair[1];
            let Some(chunks) = self.chunks_between(via_segment_id, enter_connector, exit_connector)
            else {
                return;
            };
            via_way_ids.extend(chunks);
        }

        let (final_connector, to_segment_id) = transition.sequence.last().unwrap();
        let Some(to_way_id) = self.chunk_from_connector(
            to_segment_id,
            final_connector,
            transition.final_heading.as_deref(),
        ) else {
            return;
        };

        // The chunk before the entry connector is traversed when arriving in
        // the segment's forward heading; the chunk after when arriving
        // backward. No heading scope means the transition applies to both.
        let mut from_way_ids = Vec::new();
        match transition.heading.as_deref() {
            Some("forward") => from_way_ids.extend(
                entry_position
                    .checked_sub(1)
                    .map(|i| from_topology.chunk_way_ids[i]),
            ),
            Some("backward") => {
                from_way_ids.extend(from_topology.chunk_way_ids.get(entry_position).copied())
            }
            _ => {
                from_way_ids.extend(
                    entry_position
                        .checked_sub(1)
                        .map(|i| from_topology.chunk_way_ids[i]),
                );
                from_way_ids.extend(from_topology.chunk_way_ids.get(entry_position).copied());
            }
        }

        let via_node = from_topology.cut_nodes[entry_position];
        for from_way_id in from_way_ids {
            candidates.push(TurnRestrictionCandidate {
                relation_id,
                from_way_id,
                via: if via_way_ids.is_empty() {
                    ViaSpec::Node(via_node)
                } else {
                    ViaSpec::Ways(via_way_ids.clone())
                },
                to_way_id,
                kind: RestrictionKind::NoTurn,
                mode_mask: transition.mode_mask,
            });
        }
    }

    /// Ordered chunk way ids of `segment_id` between two of its connectors.
    fn chunks_between(
        &self,
        segment_id: &str,
        from_connector: &str,
        to_connector: &str,
    ) -> Option<Vec<i64>> {
        let topology = self.segment_topologies.get(segment_id)?;
        let from = topology.connector_position(from_connector)?;
        let to = topology.connector_position(to_connector)?;
        if from == to {
            return None;
        }
        let mut chunks: Vec<i64> = if from < to {
            topology.chunk_way_ids.get(from..to)?.to_vec()
        } else {
            let mut reversed = topology.chunk_way_ids.get(to..from)?.to_vec();
            reversed.reverse();
            reversed
        };
        (!chunks.is_empty()).then(|| std::mem::take(&mut chunks))
    }

    /// The chunk of `segment_id` leaving `connector_id` when traveling in
    /// `heading` direction along the segment.
    fn chunk_from_connector(
        &self,
        segment_id: &str,
        connector_id: &str,
        heading: Option<&str>,
    ) -> Option<i64> {
        let topology = self.segment_topologies.get(segment_id)?;
        let position = topology.connector_position(connector_id)?;
        match heading {
            Some("backward") => position
                .checked_sub(1)
                .and_then(|index| topology.chunk_way_ids.get(index))
                .copied(),
            _ => topology.chunk_way_ids.get(position).copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{ArrayRef, BinaryArray};
    use arrow_schema::{Field, Fields, Schema};
    use netweevil_core::AccessMask;
    use std::sync::Arc;

    fn wkb_linestring(points: &[(f64, f64)]) -> Vec<u8> {
        let mut bytes = vec![1_u8];
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
        for (lon, lat) in points {
            bytes.extend_from_slice(&lon.to_le_bytes());
            bytes.extend_from_slice(&lat.to_le_bytes());
        }
        bytes
    }

    fn attribute_schema() -> Schema {
        let when = DataType::Struct(Fields::from(vec![
            Field::new("heading", DataType::Utf8, true),
            Field::new("during", DataType::Utf8, true),
            Field::new(
                "mode",
                DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
                true,
            ),
        ]));
        let speed = DataType::Struct(Fields::from(vec![
            Field::new("value", DataType::Float64, true),
            Field::new("unit", DataType::Utf8, true),
        ]));
        let between = DataType::List(Arc::new(Field::new("item", DataType::Float64, true)));
        let list_of = |fields: Vec<Field>| {
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(Fields::from(fields)),
                true,
            )))
        };
        Schema::new(vec![
            Field::new("id", DataType::Utf8, true),
            Field::new("subtype", DataType::Utf8, true),
            Field::new("class", DataType::Utf8, true),
            Field::new("subclass", DataType::Utf8, true),
            Field::new(
                "names",
                DataType::Struct(Fields::from(vec![Field::new(
                    "primary",
                    DataType::Utf8,
                    true,
                )])),
                true,
            ),
            Field::new(
                "connectors",
                list_of(vec![
                    Field::new("connector_id", DataType::Utf8, true),
                    Field::new("at", DataType::Float64, true),
                ]),
                true,
            ),
            Field::new(
                "access_restrictions",
                list_of(vec![
                    Field::new("access_type", DataType::Utf8, true),
                    Field::new("when", when.clone(), true),
                    Field::new("between", between.clone(), true),
                ]),
                true,
            ),
            Field::new(
                "speed_limits",
                list_of(vec![
                    Field::new("max_speed", speed, true),
                    Field::new("when", when.clone(), true),
                    Field::new("between", between.clone(), true),
                ]),
                true,
            ),
            Field::new(
                "road_surface",
                list_of(vec![
                    Field::new("value", DataType::Utf8, true),
                    Field::new("between", between.clone(), true),
                ]),
                true,
            ),
            Field::new(
                "prohibited_transitions",
                list_of(vec![
                    Field::new(
                        "sequence",
                        list_of(vec![
                            Field::new("connector_id", DataType::Utf8, true),
                            Field::new("segment_id", DataType::Utf8, true),
                        ]),
                        true,
                    ),
                    Field::new("final_heading", DataType::Utf8, true),
                    Field::new("when", when, true),
                ]),
                true,
            ),
            Field::new(
                "lanes",
                list_of(vec![Field::new("direction", DataType::Utf8, true)]),
                true,
            ),
        ])
    }

    /// Writes segments (attribute JSON + WKB geometry) as a GeoParquet file.
    fn write_fixture(rows: &[(serde_json::Value, Vec<(f64, f64)>)]) -> std::path::PathBuf {
        let attribute_schema = Arc::new(attribute_schema());
        let ndjson = rows
            .iter()
            .map(|(attributes, _)| attributes.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut decoder = arrow_json::ReaderBuilder::new(Arc::clone(&attribute_schema))
            .build(std::io::Cursor::new(ndjson.as_bytes()))
            .expect("json reader builds");
        let attribute_batch = decoder
            .next()
            .expect("one attribute batch")
            .expect("attribute batch decodes");

        let geometry: ArrayRef = Arc::new(BinaryArray::from_iter_values(
            rows.iter().map(|(_, line)| wkb_linestring(line)),
        ));
        let mut fields: Vec<Field> = attribute_schema
            .fields()
            .iter()
            .map(|field| field.as_ref().clone())
            .collect();
        fields.push(Field::new("geometry", DataType::Binary, true));
        let mut columns = attribute_batch.columns().to_vec();
        columns.push(geometry);
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("batch assembles");

        static FIXTURE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "{}-{}",
            std::process::id(),
            FIXTURE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(format!("netweevil-overture-{unique}.parquet"));
        let file = std::fs::File::create(&path).expect("fixture file creates");
        let mut writer =
            parquet::arrow::ArrowWriter::try_new(file, schema, None).expect("writer builds");
        writer.write(&batch).expect("batch writes");
        writer.close().expect("writer closes");
        path
    }

    fn scan_fixture(rows: &[(serde_json::Value, Vec<(f64, f64)>)]) -> ScanOutput {
        let path = write_fixture(rows);
        let output = scan_overture(&path, 0, &mut |_| {}).expect("fixture scans");
        std::fs::remove_file(path).expect("fixture removes");
        output
    }

    fn residential(id: &str, connectors: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "subtype": "road",
            "class": "residential",
            "connectors": connectors,
        })
    }

    #[test]
    fn parses_wkb_linestrings_in_both_byte_orders() {
        let little = wkb_linestring(&[(6.5, 53.2), (6.6, 53.3)]);
        assert_eq!(
            parse_wkb_linestring(&little),
            Some(vec![(6.5, 53.2), (6.6, 53.3)])
        );

        let mut big = vec![0_u8];
        big.extend_from_slice(&2_u32.to_be_bytes());
        big.extend_from_slice(&2_u32.to_be_bytes());
        for value in [6.5_f64, 53.2, 6.6, 53.3] {
            big.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(
            parse_wkb_linestring(&big),
            Some(vec![(6.5, 53.2), (6.6, 53.3)])
        );

        // A WKB point is not a usable segment geometry.
        let mut point = vec![1_u8];
        point.extend_from_slice(&1_u32.to_le_bytes());
        point.extend_from_slice(&6.5_f64.to_le_bytes());
        point.extend_from_slice(&53.2_f64.to_le_bytes());
        assert_eq!(parse_wkb_linestring(&point), None);
    }

    #[test]
    fn joins_segments_through_shared_connectors() {
        let mut row_a = residential(
            "seg-a",
            serde_json::json!([
                {"connector_id": "c1", "at": 0.0},
                {"connector_id": "c2", "at": 1.0},
            ]),
        );
        row_a["names"] = serde_json::json!({"primary": "Main Street"});
        row_a["speed_limits"] = serde_json::json!([{"max_speed": {"value": 50.0, "unit": "km/h"}}]);
        let row_b = residential(
            "seg-b",
            serde_json::json!([
                {"connector_id": "c2", "at": 0.0},
                {"connector_id": "c3", "at": 1.0},
            ]),
        );

        let output = scan_fixture(&[
            (row_a, vec![(6.560, 53.20), (6.561, 53.20), (6.562, 53.20)]),
            (row_b, vec![(6.562, 53.20), (6.562, 53.21)]),
        ]);

        assert_eq!(output.pending_ways.len(), 2);
        let chunk_a = &output.pending_ways[0];
        let chunk_b = &output.pending_ways[1];
        assert_eq!(chunk_a.node_ids.len(), 3);
        assert_eq!(chunk_a.node_ids.last(), chunk_b.node_ids.first());
        assert_eq!(chunk_a.name.as_deref(), Some("Main Street"));
        assert!(chunk_a.forward_access_mask.contains(AccessMask::CAR));
        assert!(chunk_a.reverse_access_mask.contains(AccessMask::CAR));
        assert_eq!(chunk_a.forward_max_speed_kph, Some(50.0));
        assert_eq!(chunk_a.reverse_max_speed_kph, Some(50.0));
        assert_eq!(output.node_coords.len(), 4);
        let (lon, lat) = output.node_coords[chunk_a.node_ids.last().unwrap()];
        assert!((lon - 6.562).abs() < 1e-9 && (lat - 53.20).abs() < 1e-9);
    }

    #[test]
    fn applies_heading_denies_and_scoped_rules_per_chunk() {
        let mut row = residential(
            "seg-a",
            serde_json::json!([
                {"connector_id": "c1", "at": 0.0},
                {"connector_id": "c2", "at": 0.5},
                {"connector_id": "c3", "at": 1.0},
            ]),
        );
        row["access_restrictions"] = serde_json::json!([
            {"access_type": "denied", "when": {"heading": "backward"}},
        ]);
        row["speed_limits"] = serde_json::json!([
            {"max_speed": {"value": 30.0, "unit": "km/h"}, "between": [0.0, 0.5]},
            {"max_speed": {"value": 50.0, "unit": "mph"}, "between": [0.5, 1.0]},
        ]);
        row["lanes"] = serde_json::json!([
            {"direction": "forward"},
            {"direction": "forward"},
            {"direction": "backward"},
        ]);

        let output = scan_fixture(&[(row, vec![(6.560, 53.20), (6.561, 53.20), (6.562, 53.20)])]);

        assert_eq!(output.pending_ways.len(), 2);
        let first = &output.pending_ways[0];
        let second = &output.pending_ways[1];
        assert_eq!(first.reverse_access_mask.0, 0);
        assert_eq!(second.reverse_access_mask.0, 0);
        assert_eq!(first.forward_max_speed_kph, Some(30.0));
        assert!((second.forward_max_speed_kph.unwrap() - 80.4672).abs() < 1e-3);
        assert_eq!(first.forward_lanes, Some(2));
        assert_eq!(first.reverse_lanes, Some(1));
    }

    #[test]
    fn skips_non_road_and_conditional_rules() {
        let mut rail = residential(
            "seg-rail",
            serde_json::json!([
                {"connector_id": "r1", "at": 0.0},
                {"connector_id": "r2", "at": 1.0},
            ]),
        );
        rail["subtype"] = serde_json::json!("rail");
        let mut conditional = residential(
            "seg-a",
            serde_json::json!([
                {"connector_id": "c1", "at": 0.0},
                {"connector_id": "c2", "at": 1.0},
            ]),
        );
        conditional["access_restrictions"] = serde_json::json!([
            {"access_type": "denied", "when": {"during": "Mo-Fr 07:00-09:00"}},
        ]);

        let output = scan_fixture(&[
            (rail, vec![(6.50, 53.20), (6.51, 53.20)]),
            (conditional, vec![(6.560, 53.20), (6.561, 53.20)]),
        ]);

        assert_eq!(output.pending_ways.len(), 1);
        let chunk = &output.pending_ways[0];
        // The time-scoped deny is not applied to the static edge.
        assert!(chunk.forward_access_mask.contains(AccessMask::CAR));
    }

    #[test]
    fn imports_overture_fixture_end_to_end() {
        let mut row = residential(
            "seg-a",
            serde_json::json!([
                {"connector_id": "c1", "at": 0.0},
                {"connector_id": "c2", "at": 1.0},
            ]),
        );
        row["speed_limits"] = serde_json::json!([{"max_speed": {"value": 60.0, "unit": "km/h"}}]);
        let fixture = write_fixture(&[(row, vec![(6.560, 53.20), (6.562, 53.20)])]);

        let workspace =
            std::env::temp_dir().join(format!("netweevil-overture-import-{}", std::process::id()));
        std::fs::create_dir_all(&workspace).expect("workspace creates");
        let paths =
            netweevil_persist::WorkspacePaths::discover(&workspace).expect("workspace discovers");
        let manifest = crate::import_dataset(
            &paths,
            &fixture,
            crate::DatasetImportOptions {
                name: "overture-test".to_string(),
                source: fixture.display().to_string(),
                format: None,
            },
        )
        .expect("overture dataset imports");

        assert_eq!(
            manifest.source_format,
            netweevil_core::SourceFormat::OvertureParquet
        );
        let meta = manifest.topology_meta.expect("topology meta present");
        assert_eq!(meta.node_count, 2);
        assert_eq!(meta.edge_count, 2);

        let bundle = netweevil_persist::read_topology_bundle(
            &manifest.topology_bundle.expect("bundle ref").path,
        )
        .expect("bundle reads back");
        assert_eq!(bundle.schema_version, 10);
        assert_eq!(bundle.edge_profile(0).max_speed_kph, Some(60.0));

        std::fs::remove_file(fixture).expect("fixture removes");
        std::fs::remove_dir_all(workspace).expect("workspace removes");
    }

    #[test]
    fn maps_prohibited_transitions_to_restriction_candidates() {
        let mut row_a = residential(
            "seg-a",
            serde_json::json!([
                {"connector_id": "c1", "at": 0.0},
                {"connector_id": "c2", "at": 1.0},
            ]),
        );
        row_a["prohibited_transitions"] = serde_json::json!([
            {
                "sequence": [{"connector_id": "c2", "segment_id": "seg-b"}],
                "final_heading": "forward",
            },
        ]);
        let row_b = residential(
            "seg-b",
            serde_json::json!([
                {"connector_id": "c2", "at": 0.0},
                {"connector_id": "c3", "at": 1.0},
            ]),
        );

        let output = scan_fixture(&[
            (row_a, vec![(6.560, 53.20), (6.562, 53.20)]),
            (row_b, vec![(6.562, 53.20), (6.562, 53.21)]),
        ]);

        assert_eq!(output.restriction_candidates.len(), 1);
        let candidate = &output.restriction_candidates[0];
        let from_way = output.pending_ways[0].osm_way_id;
        let to_way = output.pending_ways[1].osm_way_id;
        assert_eq!(candidate.from_way_id, from_way);
        assert_eq!(candidate.to_way_id, to_way);
        let shared_node = *output.pending_ways[0].node_ids.last().unwrap();
        assert!(matches!(candidate.via, ViaSpec::Node(node) if node == shared_node));

        // The candidate resolves through the shared topology build.
        let (bundle, _, _) = crate::topology::build_topology_from_scan(
            Path::new("fixture.parquet"),
            "sha",
            output,
            &mut |_| {},
        )
        .expect("topology builds");
        assert_eq!(bundle.turn_restrictions.len(), 1);
        assert_eq!(bundle.turn_restrictions[0].edge_path.len(), 2);
    }
}
