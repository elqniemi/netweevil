use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use netweevil_core::{
    AccessMask, EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, FeatureAttributeColumn,
    FeatureAttributeColumnData, FeatureAttributeDefinition, FeatureAttributeTable,
    FeatureAttributeType, HighwayClass, SmoothnessClass, SurfaceClass, TemporalRuleSet,
    normalize_attribute_name,
};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};
use crate::mapping::{DirectionValue, FieldMapping, GeoPackageLayerMapping, GeoPackageMapping};
use crate::opening_hours::parse_access_schedule;
use crate::scan::{ObjectCounts, PendingWay, ScanOutput};
use crate::wkb::parse_geopackage_lines;

pub(crate) const SOURCE_GEOMETRY_LAYOUT_COLUMN: &str = "__netweevil_geometry_layout";
pub(crate) const SOURCE_LAYER_SCHEMA_COLUMN: &str = "__netweevil_source_layer_schema";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceGeometryLayout {
    pub(crate) parts: Vec<SourceGeometryPartLayout>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceGeometryPartLayout {
    pub(crate) vertex_count: u32,
    /// Vertex indexes whose quantized node is identical to the preceding
    /// vertex. Topology intentionally omits these zero-length self loops;
    /// retaining their positions keeps the source chain reconstructible.
    pub(crate) collapsed_vertex_indexes: Vec<u32>,
    /// Exact raw coordinates that differ from the canonical coordinate of
    /// their welded topology node. These are normally absent and make
    /// quantization lossless without duplicating ordinary vertices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) coordinate_overrides: Vec<SourceGeometryCoordinateOverride>,
    /// An edge-less part has no segment from which its location can be
    /// reconstructed, so retain its exact first coordinate explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) edge_less_anchor: Option<SourceGeometryCoordinate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceGeometryCoordinateOverride {
    pub(crate) vertex_index: u32,
    pub(crate) coordinate: SourceGeometryCoordinate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceGeometryCoordinate {
    pub(crate) lon: f64,
    pub(crate) lat: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) z: Option<f64>,
}

impl From<[f64; 3]> for SourceGeometryCoordinate {
    fn from([lon, lat, z]: [f64; 3]) -> Self {
        Self {
            lon,
            lat,
            z: z.is_finite().then_some(z),
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedLayer {
    source: PathBuf,
    table: String,
    geometry_column: String,
    srs_id: i32,
    z: i32,
    mapping: GeoPackageLayerMapping,
    columns: Vec<SourceColumn>,
    row_count: u64,
}

#[derive(Debug, Clone, Serialize)]
struct SourceColumn {
    name: String,
    value_type: FeatureAttributeType,
    semantic_role: Option<String>,
    domain: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct SourceLayerSchemaDescriptor<'a> {
    table: &'a str,
    geometry_column: &'a str,
    srs_id: i32,
    z: i32,
    columns: &'a [SourceColumn],
}

#[derive(Debug, Clone)]
enum OwnedValue {
    Null,
    Integer(i64),
    Float(f64),
    String(String),
    Blob(Vec<u8>),
}

impl OwnedValue {
    fn canonical_string(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Integer(value) => Some(value.to_string()),
            Self::Float(value) if value.fract() == 0.0 => Some(format!("{value:.0}")),
            Self::Float(value) => Some(value.to_string()),
            Self::String(value) => Some(value.clone()),
            Self::Blob(value) => Some(format!("0x{}", hex::encode(value))),
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Float(value) if value.fract() == 0.0 => Some(*value as i64),
            Self::String(value) => value.trim().parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct QuantizedCoordinate(i64, i64, i64);

pub(crate) fn scan_geopackages(
    source_paths: &[PathBuf],
    mapping: &GeoPackageMapping,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<ScanOutput> {
    mapping.validate()?;
    if source_paths.is_empty() {
        bail!("GeoPackage import requires at least one source");
    }

    let mut sorted_sources = source_paths.to_vec();
    sorted_sources.sort();
    sorted_sources.dedup();
    let layers = resolve_layers(&sorted_sources, mapping)?;
    let total_rows = layers.iter().map(|layer| layer.row_count).sum::<u64>();
    let definitions = merge_attribute_definitions(&layers)?;
    let mut attributes = AttributeTableBuilder::new(definitions);
    let mut pending_ways = Vec::new();
    let mut node_coords = FxHashMap::default();
    let mut quantized_nodes = FxHashMap::default();
    let mut next_node_id = 1_i64;
    let mut feature_ids = BTreeSet::new();
    let mut temporal_rule_sets = Vec::<TemporalRuleSet>::new();
    let mut reporter = PercentReporter::starting_at_zero();
    let mut processed = 0_u64;

    emit_progress(
        progress,
        DatasetImportStage::ScanRoutableObjects,
        Some(0.0),
        format!(
            "Scanning {} GeoPackage layer(s) across {} source(s) 0%",
            layers.len(),
            sorted_sources.len()
        ),
    );

    for layer in &layers {
        let connection = open_read_only(&layer.source)?;
        let feature_id_column = layer
            .mapping
            .field_for_role("feature_id")
            .map(|(name, _)| name)
            .expect("validated feature_id role");
        let mut selection = layer
            .columns
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>();
        selection.push(quote_identifier(&layer.geometry_column));
        let sql = format!(
            "SELECT {} FROM {} ORDER BY {}",
            selection.join(", "),
            quote_identifier(&layer.table),
            quote_identifier(feature_id_column)
        );
        let mut statement = connection.prepare(&sql).with_context(|| {
            format!(
                "preparing GeoPackage layer {} in {}",
                layer.table,
                layer.source.display()
            )
        })?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let mut values = BTreeMap::<String, OwnedValue>::new();
            for (index, column) in layer.columns.iter().enumerate() {
                values.insert(
                    normalize_attribute_name(&column.name),
                    owned_sqlite_value(row.get_ref(index)?),
                );
            }
            let feature_id = value_for_source_column(&values, feature_id_column)
                .and_then(OwnedValue::as_i64)
                .with_context(|| {
                    format!(
                        "feature_id column '{}' in {}.{} is null or not an integer",
                        feature_id_column,
                        layer.source.display(),
                        layer.table
                    )
                })?;
            if !feature_ids.insert(feature_id) {
                bail!(
                    "source feature id {feature_id} occurs more than once across GeoPackage inputs; feature-id namespaces must be disjoint"
                );
            }

            let direction = mapped_direction(&values, layer)?;
            let force_both = mapping
                .materialize_both_directions_for
                .contains(&feature_id);
            let access_mask = access_mask(&layer.mapping.defaults.access)?;
            let (
                forward_access_mask,
                reverse_access_mask,
                forward_extra_flags,
                reverse_extra_flags,
            ) = match (direction, force_both) {
                (DirectionValue::Forward, true) => (
                    access_mask,
                    access_mask,
                    0,
                    EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
                ),
                (DirectionValue::Reverse, true) => (
                    access_mask,
                    access_mask,
                    EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
                    0,
                ),
                (DirectionValue::Both, _) => (access_mask, access_mask, 0, 0),
                (DirectionValue::Forward, false) => (access_mask, AccessMask::default(), 0, 0),
                (DirectionValue::Reverse, false) => (AccessMask::default(), access_mask, 0, 0),
            };
            let highway = mapped_highway(&values, layer)?;
            let name = mapped_name(&values, layer);
            let temporal_rule_id = mapped_schedule(&values, layer)?
                .map(|rule_set| intern_rule_set(&mut temporal_rule_sets, rule_set));

            let geometry_index = layer.columns.len();
            let geometry = match row.get_ref(geometry_index)? {
                ValueRef::Blob(value) => {
                    let geometry = parse_geopackage_lines(value).with_context(|| {
                        format!(
                            "parsing geometry for feature {feature_id} in {}.{}",
                            layer.source.display(),
                            layer.table
                        )
                    })?;
                    if geometry.srs_id != layer.srs_id {
                        bail!(
                            "feature {feature_id} geometry SRS {} differs from gpkg_geometry_columns SRS {}",
                            geometry.srs_id,
                            layer.srs_id
                        );
                    }
                    Some(geometry)
                }
                ValueRef::Null => None,
                _ => bail!(
                    "geometry column '{}.{}' is not a GeoPackage geometry BLOB",
                    layer.table,
                    layer.geometry_column
                ),
            };
            let raw_lines = geometry.map(|geometry| geometry.lines).unwrap_or_default();
            let mut node_chains = Vec::with_capacity(raw_lines.len());
            for line in &raw_lines {
                let mut node_ids = Vec::with_capacity(line.len());
                for &[lon, lat, z] in line {
                    validate_coordinate(lon, lat, z, layer, feature_id, mapping)?;
                    let key = quantized_coordinate(lon, lat, z, mapping)?;
                    let node_id = if let Some(node_id) = quantized_nodes.get(&key).copied() {
                        node_id
                    } else {
                        let node_id = next_node_id;
                        next_node_id = next_node_id.checked_add(1).ok_or_else(|| {
                            anyhow::anyhow!("synthetic GeoPackage node id overflow")
                        })?;
                        quantized_nodes.insert(key, node_id);
                        node_coords.insert(node_id, (lon, lat, z));
                        node_id
                    };
                    node_ids.push(node_id);
                }
                node_chains.push(node_ids);
            }
            let layout = source_geometry_layout(&raw_lines, &node_chains, &node_coords)?;
            values.insert(
                normalize_attribute_name(SOURCE_GEOMETRY_LAYOUT_COLUMN),
                OwnedValue::String(serde_json::to_string(&layout)?),
            );
            values.insert(
                normalize_attribute_name(SOURCE_LAYER_SCHEMA_COLUMN),
                OwnedValue::String(source_layer_schema(layer)?),
            );
            let feature_row = attributes.append_row(&values)?;

            for node_ids in node_chains {
                if node_ids.len() < 2 {
                    continue;
                }
                if node_ids.windows(2).all(|pair| pair[0] == pair[1]) {
                    continue;
                }
                pending_ways.push(PendingWay {
                    osm_way_id: feature_id,
                    node_ids,
                    road_class: highway.road_class(),
                    highway,
                    duration_s: None,
                    surface: SurfaceClass::Unknown,
                    smoothness: SmoothnessClass::Unknown,
                    forward_access_mask,
                    reverse_access_mask,
                    forward_destination_only: AccessMask::default(),
                    reverse_destination_only: AccessMask::default(),
                    forward_conditional_access: AccessMask::default(),
                    reverse_conditional_access: AccessMask::default(),
                    is_toll: false,
                    is_roundabout: false,
                    forward_extra_flags,
                    reverse_extra_flags,
                    forward_max_speed_kph: None,
                    reverse_max_speed_kph: None,
                    forward_lanes: None,
                    reverse_lanes: None,
                    name: name.clone(),
                    guidance: None,
                    feature_row,
                    forward_temporal_rule_id: temporal_rule_id,
                    reverse_temporal_rule_id: temporal_rule_id,
                    vehicle_restrictions: Vec::new(),
                });
            }

            processed += 1;
            reporter.emit_if_needed(
                processed,
                total_rows,
                DatasetImportStage::ScanRoutableObjects,
                progress,
                |percent| {
                    format!(
                        "Scanning GeoPackage features {:.0}% ({processed}/{total_rows}, {} segments)",
                        percent,
                        pending_ways.len()
                    )
                },
            );
        }
    }

    let feature_attributes = attributes.finish()?;
    emit_progress(
        progress,
        DatasetImportStage::ScanRoutableObjects,
        Some(100.0),
        format!(
            "Scanning GeoPackage features 100% ({} features, {} segment chains, {} welded nodes)",
            feature_ids.len(),
            pending_ways.len(),
            node_coords.len()
        ),
    );

    Ok(ScanOutput {
        pending_ways,
        restriction_candidates: Vec::new(),
        node_coords,
        traffic_signal_nodes: FxHashSet::default(),
        junction_nodes: Default::default(),
        counts: ObjectCounts {
            nodes: quantized_nodes.len() as u64,
            ways: feature_ids.len() as u64,
            relations: 0,
        },
        feature_attributes,
        temporal_rule_sets,
    })
}

fn resolve_layers(
    source_paths: &[PathBuf],
    mapping: &GeoPackageMapping,
) -> Result<Vec<ResolvedLayer>> {
    let mut layers = Vec::new();
    let mut matched = vec![0_usize; mapping.layers.len()];
    for source in source_paths {
        let connection = open_read_only(source)?;
        let mut statement = connection.prepare(
            "SELECT table_name, column_name, srs_id, z FROM gpkg_geometry_columns ORDER BY table_name",
        ).with_context(|| format!("reading gpkg_geometry_columns from {}", source.display()))?;
        let geometry_rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i32>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (table, geometry_column, srs_id, z) in geometry_rows {
            let Some((mapping_index, layer_mapping)) = mapping
                .layers
                .iter()
                .enumerate()
                .find(|(_, layer)| layer_matches(layer, source, &table))
            else {
                continue;
            };
            matched[mapping_index] += 1;
            if let Some(expected) = layer_mapping.geometry_column.as_deref()
                && !expected.eq_ignore_ascii_case(&geometry_column)
            {
                bail!(
                    "mapped geometry column '{}.{}' differs from GeoPackage metadata column '{}'",
                    table,
                    expected,
                    geometry_column
                );
            }
            let domains = read_domains(&connection, &table)?;
            let mut columns = Vec::new();
            let mut column_statement =
                connection.prepare("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")?;
            let column_rows = column_statement
                .query_map([&table], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (name, declared_type) in column_rows {
                if name.eq_ignore_ascii_case(&geometry_column) {
                    continue;
                }
                let field_mapping = field_mapping_for_name(layer_mapping, &name);
                let semantic_role = field_mapping.map(|field| field.role().to_string());
                let mut domain = domains
                    .get(&normalize_attribute_name(&name))
                    .cloned()
                    .unwrap_or_default();
                if let Some(decode) = field_mapping.and_then(FieldMapping::decode) {
                    domain.extend(decode.clone());
                }
                columns.push(SourceColumn {
                    name,
                    value_type: sqlite_attribute_type(&declared_type, semantic_role.as_deref()),
                    semantic_role,
                    domain,
                });
            }
            let count_sql = format!("SELECT COUNT(*) FROM {}", quote_identifier(&table));
            let row_count = connection.query_row(&count_sql, [], |row| row.get::<_, u64>(0))?;
            layers.push(ResolvedLayer {
                source: source.clone(),
                table,
                geometry_column,
                srs_id,
                z,
                mapping: layer_mapping.clone(),
                columns,
                row_count,
            });
        }
    }
    for (index, count) in matched.into_iter().enumerate() {
        if count == 0 {
            bail!(
                "mapped GeoPackage layer '{}' was not found in any input source",
                mapping.layers[index].table
            );
        }
    }
    if layers.is_empty() {
        bail!("none of the mapped GeoPackage layers were found in the input sources");
    }
    layers.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.table.cmp(&right.table))
    });
    Ok(layers)
}

fn layer_matches(mapping: &GeoPackageLayerMapping, source: &Path, table: &str) -> bool {
    if !mapping.table.eq_ignore_ascii_case(table) {
        return false;
    }
    mapping.source.as_deref().is_none_or(|expected| {
        source
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            || source.to_string_lossy().ends_with(expected)
    })
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening GeoPackage {}", path.display()))
}

fn read_domains(
    connection: &Connection,
    table: &str,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    if !sqlite_table_exists(connection, "gpkg_data_columns")?
        || !sqlite_table_exists(connection, "gpkg_data_column_constraints")?
    {
        return Ok(BTreeMap::new());
    }
    let mut statement = connection.prepare(
        "SELECT dc.column_name, cc.value, cc.description
         FROM gpkg_data_columns dc
         JOIN gpkg_data_column_constraints cc ON cc.constraint_name = dc.constraint_name
         WHERE dc.table_name = ?1 AND cc.constraint_type = 'enum'
         ORDER BY dc.column_name, cc.value",
    )?;
    let mut domains = BTreeMap::<String, BTreeMap<String, String>>::new();
    let rows = statement.query_map([table], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (column, value, description) = row?;
        domains
            .entry(normalize_attribute_name(&column))
            .or_default()
            .insert(value.clone(), description.unwrap_or(value));
    }
    Ok(domains)
}

fn sqlite_table_exists(connection: &Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn field_mapping_for_name<'a>(
    mapping: &'a GeoPackageLayerMapping,
    name: &str,
) -> Option<&'a FieldMapping> {
    mapping
        .fields
        .iter()
        .find(|(column, _)| column.eq_ignore_ascii_case(name))
        .map(|(_, mapping)| mapping)
}

fn sqlite_attribute_type(declared: &str, role: Option<&str>) -> FeatureAttributeType {
    if role.is_some_and(|role| role.eq_ignore_ascii_case("access_schedule")) {
        return FeatureAttributeType::Json;
    }
    let declared = declared.to_ascii_uppercase();
    if declared.contains("BOOL") {
        FeatureAttributeType::Boolean
    } else if declared.contains("INT") {
        FeatureAttributeType::Integer
    } else if declared.contains("REAL")
        || declared.contains("FLOA")
        || declared.contains("DOUB")
        || declared.contains("DEC")
        || declared.contains("NUM")
    {
        FeatureAttributeType::Float
    } else {
        FeatureAttributeType::String
    }
}

fn merge_attribute_definitions(
    layers: &[ResolvedLayer],
) -> Result<Vec<FeatureAttributeDefinition>> {
    let mut definitions = BTreeMap::<String, FeatureAttributeDefinition>::new();
    for layer in layers {
        for column in &layer.columns {
            let key = normalize_attribute_name(&column.name);
            if let Some(existing) = definitions.get_mut(&key) {
                if existing.name != column.name {
                    bail!(
                        "source columns '{}' and '{}' collide after attribute-name normalization; rename or map them distinctly before lossless import",
                        existing.name,
                        column.name
                    );
                }
                existing.value_type = promote_type(existing.value_type, column.value_type);
                if let Some(role) = column.semantic_role.as_deref() {
                    if let Some(existing_role) = existing.semantic_role.as_deref()
                        && !existing_role.eq_ignore_ascii_case(role)
                    {
                        bail!(
                            "source column '{}' maps to conflicting semantic roles '{}' and '{}'",
                            column.name,
                            existing_role,
                            role
                        );
                    }
                    existing
                        .semantic_role
                        .get_or_insert_with(|| role.to_string());
                }
                existing.domain.extend(column.domain.clone());
            } else {
                definitions.insert(
                    key,
                    FeatureAttributeDefinition {
                        name: column.name.clone(),
                        semantic_role: column.semantic_role.clone(),
                        value_type: column.value_type,
                        domain: column.domain.clone(),
                    },
                );
            }
        }
    }
    for reserved in [SOURCE_GEOMETRY_LAYOUT_COLUMN, SOURCE_LAYER_SCHEMA_COLUMN] {
        let key = normalize_attribute_name(reserved);
        if definitions.contains_key(&key) {
            bail!(
                "source attribute name '{}' is reserved for lossless ingest metadata",
                reserved
            );
        }
        definitions.insert(
            key,
            FeatureAttributeDefinition {
                name: reserved.to_string(),
                semantic_role: None,
                value_type: FeatureAttributeType::Json,
                domain: BTreeMap::new(),
            },
        );
    }
    Ok(definitions.into_values().collect())
}

fn source_layer_schema(layer: &ResolvedLayer) -> Result<String> {
    serde_json::to_string(&SourceLayerSchemaDescriptor {
        table: &layer.table,
        geometry_column: &layer.geometry_column,
        srs_id: layer.srs_id,
        z: layer.z,
        columns: &layer.columns,
    })
    .context("serializing GeoPackage source-layer schema metadata")
}

fn source_geometry_layout(
    raw_lines: &[Vec<[f64; 3]>],
    node_chains: &[Vec<i64>],
    node_coords: &FxHashMap<i64, (f64, f64, f64)>,
) -> Result<SourceGeometryLayout> {
    if raw_lines.len() != node_chains.len() {
        bail!("internal GeoPackage geometry layout line/node count mismatch");
    }
    let parts = raw_lines
        .iter()
        .zip(node_chains)
        .map(|(coordinates, nodes)| {
            let vertex_count = u32::try_from(nodes.len())
                .context("GeoPackage geometry part has more than u32::MAX vertices")?;
            let collapsed_vertex_indexes = nodes
                .windows(2)
                .enumerate()
                .filter_map(|(index, pair)| (pair[0] == pair[1]).then_some(index + 1))
                .map(|index| {
                    u32::try_from(index)
                        .context("GeoPackage geometry vertex index exceeds u32::MAX")
                })
                .collect::<Result<Vec<_>>>()?;
            let coordinate_overrides = coordinates
                .iter()
                .zip(nodes)
                .enumerate()
                .map(|(vertex_index, (coordinate, node_id))| {
                    let canonical = node_coords.get(node_id).copied().with_context(|| {
                        format!("geometry layout references missing synthetic node {node_id}")
                    })?;
                    let canonical = [canonical.0, canonical.1, canonical.2];
                    if coordinates_equal(*coordinate, canonical) {
                        Ok(None)
                    } else {
                        Ok(Some(SourceGeometryCoordinateOverride {
                            vertex_index: u32::try_from(vertex_index)
                                .context("GeoPackage geometry vertex index exceeds u32::MAX")?,
                            coordinate: (*coordinate).into(),
                        }))
                    }
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect();
            let edge_less_anchor = nodes
                .first()
                .zip(coordinates.first())
                .filter(|_| nodes.len() < 2 || nodes.windows(2).all(|pair| pair[0] == pair[1]))
                .map(|(_, coordinate)| (*coordinate).into());
            Ok(SourceGeometryPartLayout {
                vertex_count,
                collapsed_vertex_indexes,
                coordinate_overrides,
                edge_less_anchor,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SourceGeometryLayout { parts })
}

fn coordinates_equal(left: [f64; 3], right: [f64; 3]) -> bool {
    left.into_iter()
        .zip(right)
        .all(|(left, right)| left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan()))
}

fn promote_type(left: FeatureAttributeType, right: FeatureAttributeType) -> FeatureAttributeType {
    use FeatureAttributeType::*;
    match (left, right) {
        (value, other) if value == other => value,
        (Integer, Float) | (Float, Integer) => Float,
        (Json, String) | (String, Json) => Json,
        _ => String,
    }
}

fn owned_sqlite_value(value: ValueRef<'_>) -> OwnedValue {
    match value {
        ValueRef::Null => OwnedValue::Null,
        ValueRef::Integer(value) => OwnedValue::Integer(value),
        ValueRef::Real(value) => OwnedValue::Float(value),
        ValueRef::Text(value) => match String::from_utf8(value.to_vec()) {
            Ok(value) => OwnedValue::String(value),
            Err(_) => OwnedValue::String(format!("0x{}", hex::encode(value))),
        },
        ValueRef::Blob(value) => OwnedValue::Blob(value.to_vec()),
    }
}

fn value_for_source_column<'a>(
    values: &'a BTreeMap<String, OwnedValue>,
    column: &str,
) -> Option<&'a OwnedValue> {
    values.get(&normalize_attribute_name(column))
}

fn value_for_role<'a>(
    values: &'a BTreeMap<String, OwnedValue>,
    layer: &'a ResolvedLayer,
    role: &str,
) -> Option<(&'a OwnedValue, &'a SourceColumn, Option<&'a FieldMapping>)> {
    let (name, mapping) = layer.mapping.field_for_role(role)?;
    let column = layer
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(name))?;
    Some((
        value_for_source_column(values, name)?,
        column,
        Some(mapping),
    ))
}

fn decoded_value(
    value: &OwnedValue,
    column: &SourceColumn,
    mapping: Option<&FieldMapping>,
) -> Option<String> {
    let raw = value.canonical_string()?;
    mapping
        .and_then(FieldMapping::decode)
        .and_then(|decode| decode.get(&raw))
        .or_else(|| column.domain.get(&raw))
        .cloned()
        .or(Some(raw))
}

fn mapped_direction(
    values: &BTreeMap<String, OwnedValue>,
    layer: &ResolvedLayer,
) -> Result<DirectionValue> {
    let Some((value, column, mapping)) = value_for_role(values, layer, "direction") else {
        return Ok(layer.mapping.defaults.direction);
    };
    let raw = value.canonical_string().unwrap_or_default();
    let decoded = decoded_value(value, column, mapping).unwrap_or_default();
    for candidate in [&decoded, &raw] {
        match candidate.trim() {
            "-1" => return Ok(DirectionValue::Reverse),
            "0" => return Ok(DirectionValue::Both),
            "1" => return Ok(DirectionValue::Forward),
            _ => {}
        }
        let normalized = normalize_attribute_name(candidate);
        if matches!(
            normalized.as_str(),
            "1" | "forward" | "forward_only" | "one_way_forward"
        ) {
            return Ok(DirectionValue::Forward);
        }
        if matches!(
            normalized.as_str(),
            "reverse" | "backward" | "reverse_only" | "one_way_backward"
        ) {
            return Ok(DirectionValue::Reverse);
        }
        if matches!(normalized.as_str(), "0" | "both" | "both_ways" | "two_way") {
            return Ok(DirectionValue::Both);
        }
    }
    bail!(
        "cannot decode direction value '{}' in {}.{}; add a mapping decode table",
        raw,
        layer.source.display(),
        layer.table
    )
}

fn mapped_highway(
    values: &BTreeMap<String, OwnedValue>,
    layer: &ResolvedLayer,
) -> Result<HighwayClass> {
    let value = value_for_role(values, layer, "class")
        .or_else(|| value_for_role(values, layer, "feature_type"));
    let class = value
        .and_then(|(value, column, mapping)| decoded_value(value, column, mapping))
        .unwrap_or_else(|| layer.mapping.defaults.highway.clone());
    highway_class(&class).with_context(|| {
        format!(
            "decoding class '{}' in {}.{}",
            class,
            layer.source.display(),
            layer.table
        )
    })
}

fn highway_class(value: &str) -> Result<HighwayClass> {
    let normalized = normalize_attribute_name(value);
    let normalized = normalized.strip_prefix("m_").unwrap_or(&normalized);
    Ok(match normalized {
        "motorway" => HighwayClass::Motorway,
        "motorway_link" => HighwayClass::MotorwayLink,
        "trunk" => HighwayClass::Trunk,
        "trunk_link" => HighwayClass::TrunkLink,
        "primary" => HighwayClass::Primary,
        "primary_link" => HighwayClass::PrimaryLink,
        "secondary" => HighwayClass::Secondary,
        "secondary_link" => HighwayClass::SecondaryLink,
        "tertiary" => HighwayClass::Tertiary,
        "tertiary_link" => HighwayClass::TertiaryLink,
        "residential" | "village" => HighwayClass::Residential,
        "unclassified" => HighwayClass::Unclassified,
        "living_street" => HighwayClass::LivingStreet,
        "service" | "service_lane" | "runin" => HighwayClass::Service,
        "track" => HighwayClass::Track,
        "cycleway" => HighwayClass::Cycleway,
        "pedestrian" => HighwayClass::Pedestrian,
        "steps" | "stair" | "stairs" | "staircase" | "stairlift" => HighwayClass::Steps,
        "bridleway" => HighwayClass::Bridleway,
        "ferry" => HighwayClass::Ferry,
        "path" | "footpath" | "generalized_walkway_inside_park" => HighwayClass::Path,
        "footway"
        | "footbridge"
        | "subway"
        | "traffic_island"
        | "escalator"
        | "travelator"
        | "lift"
        | "ramp"
        | "crossing_signalized"
        | "crossing_zebra"
        | "crossing_cautionary"
        | "crossing_others"
        | "other"
        | "others" => HighwayClass::Footway,
        other => bail!("unsupported highway/class value '{other}'"),
    })
}

fn access_mask(values: &[String]) -> Result<AccessMask> {
    let mut bits = 0_u16;
    for value in values {
        bits |= match normalize_attribute_name(value).as_str() {
            "car" => AccessMask::CAR,
            "bicycle" | "bike" => AccessMask::BICYCLE,
            "foot" | "pedestrian" | "walk" => AccessMask::FOOT,
            "bus" | "transit" => AccessMask::BUS,
            "hgv" | "truck" => AccessMask::HGV,
            "taxi" => AccessMask::TAXI,
            "motorcycle" => AccessMask::MOTORCYCLE,
            "moped" | "motor_scooter" => AccessMask::MOPED,
            other => bail!("unsupported layer default access mode '{other}'"),
        };
    }
    Ok(AccessMask::new(bits))
}

fn mapped_name(values: &BTreeMap<String, OwnedValue>, layer: &ResolvedLayer) -> Option<String> {
    layer
        .mapping
        .fields
        .iter()
        .filter(|(_, mapping)| normalize_attribute_name(mapping.role()).starts_with("name"))
        .filter_map(|(name, _)| value_for_source_column(values, name))
        .filter_map(OwnedValue::canonical_string)
        .find(|value| !value.trim().is_empty())
}

fn mapped_schedule(
    values: &BTreeMap<String, OwnedValue>,
    layer: &ResolvedLayer,
) -> Result<Option<TemporalRuleSet>> {
    let Some((schedule, _, _)) = value_for_role(values, layer, "access_schedule") else {
        return Ok(None);
    };
    let Some(details) = schedule.canonical_string() else {
        return Ok(None);
    };
    let polarity = [
        "access_schedule_polarity",
        "schedule_polarity",
        "access_schedule_type",
    ]
    .into_iter()
    .find_map(|role| value_for_role(values, layer, role))
    .and_then(|(value, column, mapping)| decoded_value(value, column, mapping))
    .unwrap_or_else(|| "OPEN".to_string());
    let rules = parse_access_schedule(&details, &polarity).with_context(|| {
        format!(
            "parsing mapped access schedule in {}.{}",
            layer.source.display(),
            layer.table
        )
    })?;
    Ok((!rules.rules.is_empty()).then_some(rules))
}

fn intern_rule_set(rule_sets: &mut Vec<TemporalRuleSet>, value: TemporalRuleSet) -> u32 {
    if let Some(index) = rule_sets.iter().position(|existing| existing == &value) {
        return index as u32;
    }
    let index = rule_sets.len() as u32;
    rule_sets.push(value);
    index
}

fn validate_coordinate(
    lon: f64,
    lat: f64,
    z: f64,
    layer: &ResolvedLayer,
    feature_id: i64,
    mapping: &GeoPackageMapping,
) -> Result<()> {
    if !lon.is_finite()
        || !lat.is_finite()
        || !(-180.0..=180.0).contains(&lon)
        || !(-90.0..=90.0).contains(&lat)
    {
        bail!(
            "feature {feature_id} in {}.{} has coordinate ({lon}, {lat}) outside WGS84 longitude/latitude; reproject the GeoPackage before import",
            layer.source.display(),
            layer.table
        );
    }
    if mapping.coordinates.z_metres && layer.z > 0 && !z.is_finite() {
        bail!(
            "feature {feature_id} in {}.{} declares Z coordinates but contains a non-finite elevation",
            layer.source.display(),
            layer.table
        );
    }
    Ok(())
}

fn quantized_coordinate(
    lon: f64,
    lat: f64,
    z: f64,
    mapping: &GeoPackageMapping,
) -> Result<QuantizedCoordinate> {
    fn quantize(value: f64, epsilon: f64, label: &str) -> Result<i64> {
        let quantized = (value / epsilon).round();
        if !quantized.is_finite() || quantized < i64::MIN as f64 || quantized > i64::MAX as f64 {
            bail!("{label} coordinate cannot be represented at quantization {epsilon}");
        }
        Ok(quantized as i64)
    }
    Ok(QuantizedCoordinate(
        quantize(lon, mapping.quantization.xy_degrees, "longitude")?,
        quantize(lat, mapping.quantization.xy_degrees, "latitude")?,
        if z.is_finite() {
            quantize(z, mapping.quantization.z_m, "elevation")?
        } else {
            i64::MIN
        },
    ))
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

struct AttributeTableBuilder {
    definitions: Vec<FeatureAttributeDefinition>,
    columns: Vec<MutableColumn>,
    strings: Vec<String>,
    string_lookup: BTreeMap<String, u32>,
    row_count: u32,
}

enum MutableColumn {
    Boolean(Vec<Option<bool>>),
    Integer(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
    String(Vec<Option<u32>>),
    Json(Vec<Option<u32>>),
}

impl AttributeTableBuilder {
    fn new(definitions: Vec<FeatureAttributeDefinition>) -> Self {
        let columns = definitions
            .iter()
            .map(|definition| match definition.value_type {
                FeatureAttributeType::Boolean => MutableColumn::Boolean(Vec::new()),
                FeatureAttributeType::Integer => MutableColumn::Integer(Vec::new()),
                FeatureAttributeType::Float => MutableColumn::Float(Vec::new()),
                FeatureAttributeType::String => MutableColumn::String(Vec::new()),
                FeatureAttributeType::Json => MutableColumn::Json(Vec::new()),
            })
            .collect();
        Self {
            definitions,
            columns,
            strings: Vec::new(),
            string_lookup: BTreeMap::new(),
            row_count: 0,
        }
    }

    fn append_row(&mut self, values: &BTreeMap<String, OwnedValue>) -> Result<u32> {
        let row = self.row_count;
        for index in 0..self.columns.len() {
            let key = normalize_attribute_name(&self.definitions[index].name);
            let value = values.get(&key).unwrap_or(&OwnedValue::Null);
            let string_index = match &self.columns[index] {
                MutableColumn::String(_) | MutableColumn::Json(_) => value
                    .canonical_string()
                    .map(|value| self.intern_string(value)),
                _ => None,
            };
            match &mut self.columns[index] {
                MutableColumn::Boolean(column) => column.push(match value {
                    OwnedValue::Null => None,
                    OwnedValue::Integer(value) => Some(*value != 0),
                    OwnedValue::String(value) if value.eq_ignore_ascii_case("true") => Some(true),
                    OwnedValue::String(value) if value.eq_ignore_ascii_case("false") => Some(false),
                    _ => bail!(
                        "attribute '{}' cannot be converted to boolean",
                        self.definitions[index].name
                    ),
                }),
                MutableColumn::Integer(column) => column.push(match value {
                    OwnedValue::Null => None,
                    OwnedValue::Integer(value) => Some(*value),
                    _ => bail!(
                        "attribute '{}' cannot be converted to integer",
                        self.definitions[index].name
                    ),
                }),
                MutableColumn::Float(column) => column.push(match value {
                    OwnedValue::Null => None,
                    OwnedValue::Integer(value) => Some(*value as f64),
                    OwnedValue::Float(value) => Some(*value),
                    _ => bail!(
                        "attribute '{}' cannot be converted to float",
                        self.definitions[index].name
                    ),
                }),
                MutableColumn::String(column) | MutableColumn::Json(column) => {
                    column.push(string_index)
                }
            }
        }
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("feature attribute row count exceeds u32"))?;
        Ok(row)
    }

    fn intern_string(&mut self, value: String) -> u32 {
        if let Some(index) = self.string_lookup.get(&value) {
            return *index;
        }
        let index = self.strings.len() as u32;
        self.strings.push(value.clone());
        self.string_lookup.insert(value, index);
        index
    }

    fn finish(self) -> Result<FeatureAttributeTable> {
        let columns = self
            .definitions
            .into_iter()
            .zip(self.columns)
            .map(|(definition, column)| FeatureAttributeColumn {
                definition,
                data: match column {
                    MutableColumn::Boolean(values) => FeatureAttributeColumnData::Boolean(values),
                    MutableColumn::Integer(values) => FeatureAttributeColumnData::Integer(values),
                    MutableColumn::Float(values) => FeatureAttributeColumnData::Float(values),
                    MutableColumn::String(values) => FeatureAttributeColumnData::String(values),
                    MutableColumn::Json(values) => FeatureAttributeColumnData::Json(values),
                },
            })
            .collect();
        let table = FeatureAttributeTable {
            row_count: self.row_count,
            strings: self.strings,
            columns,
        };
        table.validate().map_err(anyhow::Error::msg)?;
        Ok(table)
    }
}

#[cfg(test)]
mod tests;
