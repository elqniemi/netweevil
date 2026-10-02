use super::*;
use crate::test_util::{TempDir, gpkg_linestring_z};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ProfileDocument;
use netweevil_query::{RouteRequest, RouteResult};
use rusqlite::params;

const GEOMETRY_COLUMNS_DDL: &str = "CREATE TABLE gpkg_geometry_columns (
    table_name TEXT NOT NULL,
    column_name TEXT NOT NULL,
    geometry_type_name TEXT NOT NULL,
    srs_id INTEGER NOT NULL,
    z INTEGER NOT NULL,
    m INTEGER NOT NULL
);
INSERT INTO gpkg_geometry_columns VALUES ('walkways', 'geom', 'LINESTRING', 4326, 1, 0);";

#[test]
fn source_geometry_layout_retains_parts_and_collapsed_vertices() {
    let raw_lines = vec![
        vec![[1.0, 2.0, 3.0], [1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
        vec![[4.0, 5.0, 6.0], [7.0, 8.0, 9.0]],
    ];
    let node_chains = vec![vec![1, 1, 2], vec![2, 3]];
    let node_coords = FxHashMap::from_iter([
        (1, (1.0, 2.0, 3.0)),
        (2, (4.0, 5.0, 6.0)),
        (3, (7.0, 8.0, 9.0)),
    ]);
    let layout =
        source_geometry_layout(&raw_lines, &node_chains, &node_coords).expect("geometry layout");
    assert_eq!(
        serde_json::to_value(layout).expect("serialize layout"),
        serde_json::json!({
            "parts": [
                {"vertex_count": 3, "collapsed_vertex_indexes": [1]},
                {"vertex_count": 2, "collapsed_vertex_indexes": []}
            ]
        })
    );
}

#[test]
fn source_geometry_layout_retains_quantized_overrides_and_edge_less_anchor() {
    let raw_lines = vec![vec![[1.001, 2.0, 3.0], [1.002, 2.0, 3.0]]];
    let node_chains = vec![vec![1, 1]];
    let node_coords = FxHashMap::from_iter([(1, (1.0, 2.0, 3.0))]);
    let layout =
        source_geometry_layout(&raw_lines, &node_chains, &node_coords).expect("geometry layout");
    assert_eq!(
        serde_json::to_value(layout).expect("serialize layout"),
        serde_json::json!({
            "parts": [{
                "vertex_count": 2,
                "collapsed_vertex_indexes": [1],
                "coordinate_overrides": [
                    {"vertex_index": 0, "coordinate": {"lon": 1.001, "lat": 2.0, "z": 3.0}},
                    {"vertex_index": 1, "coordinate": {"lon": 1.002, "lat": 2.0, "z": 3.0}}
                ],
                "edge_less_anchor": {"lon": 1.001, "lat": 2.0, "z": 3.0}
            }]
        })
    );

    let two_dimensional = source_geometry_layout(
        &[vec![[1.0, 2.0, f64::NAN]]],
        &[vec![9]],
        &FxHashMap::from_iter([(9, (1.0, 2.0, f64::NAN))]),
    )
    .expect("2D edge-less layout");
    assert_eq!(
        serde_json::to_value(two_dimensional).expect("serialize 2D layout"),
        serde_json::json!({
            "parts": [{
                "vertex_count": 1,
                "collapsed_vertex_indexes": [],
                "edge_less_anchor": {"lon": 1.0, "lat": 2.0}
            }]
        })
    );
}

#[test]
fn maps_common_pedestrian_facilities_to_routing_classes() {
    assert_eq!(highway_class("Staircase").unwrap(), HighwayClass::Steps);
    assert_eq!(highway_class("M_Escalator").unwrap(), HighwayClass::Footway);
    assert_eq!(highway_class("Footbridge").unwrap(), HighwayClass::Footway);
}

#[test]
fn quote_identifier_escapes_double_quotes() {
    assert_eq!(quote_identifier("some\"column"), "\"some\"\"column\"");
}

#[test]
fn scans_mapped_3d_features_welds_nodes_and_retains_domains() {
    let dir = TempDir::new("gpkg-scan");
    let path = dir.join("walkways.gpkg");
    let connection = Connection::open(&path).expect("fixture opens");
    connection
        .execute_batch(&format!(
            "{GEOMETRY_COLUMNS_DDL}
             CREATE TABLE gpkg_data_columns (
                table_name TEXT NOT NULL,
                column_name TEXT NOT NULL,
                constraint_name TEXT
             );
             CREATE TABLE gpkg_data_column_constraints (
                constraint_name TEXT NOT NULL,
                constraint_type TEXT NOT NULL,
                value TEXT,
                description TEXT
             );
             CREATE TABLE walkways (
                id INTEGER PRIMARY KEY,
                feature_type INTEGER,
                direction INTEGER,
                schedule TEXT,
                polarity TEXT,
                covered INTEGER,
                geom BLOB
             );
             INSERT INTO gpkg_data_columns VALUES
                ('walkways', 'feature_type', 'feature_types');
             INSERT INTO gpkg_data_column_constraints VALUES
                ('feature_types', 'enum', '10', 'Lift'),
                ('feature_types', 'enum', '12', 'Staircase');"
        ))
        .expect("fixture schema");
    let schedule = r#"[{"day_code":"ED","from_time":930,"to_time":2359}]"#;
    for row in [
        params![
            100_i64,
            12_i64,
            0_i64,
            Some(schedule),
            Some("OPEN"),
            -1_i64,
            gpkg_linestring_z(&[[114.0, 22.0, 4.0], [114.001, 22.0, 5.0]])
        ],
        params![
            101_i64,
            10_i64,
            -1_i64,
            Option::<&str>::None,
            Option::<&str>::None,
            1_i64,
            gpkg_linestring_z(&[[114.001, 22.0, 5.0], [114.002, 22.0, 10.0]])
        ],
    ] {
        connection
            .execute(
                "INSERT INTO walkways VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                row,
            )
            .expect("feature inserts");
    }
    drop(connection);

    let mapping: GeoPackageMapping = serde_yaml::from_str(
        r#"
retain: all
materialize_both_directions_for: [101]
layers:
  - table: walkways
    geometry_column: geom
    fields:
      id: feature_id
      feature_type: feature_type
      direction: direction
      schedule: access_schedule
      polarity: access_schedule_polarity
      covered: covered
"#,
    )
    .expect("mapping");
    let scan = scan_geopackages(std::slice::from_ref(&path), &mapping, &mut |_| {})
        .expect("fixture scans");
    assert_eq!(scan.counts.ways, 2);
    assert_eq!(scan.node_coords.len(), 3, "shared portal must weld");
    assert_eq!(scan.pending_ways.len(), 2);
    let (staircase, lift) = (&scan.pending_ways[0], &scan.pending_ways[1]);
    assert!(staircase.forward_access_mask.contains(AccessMask::FOOT));
    assert!(staircase.reverse_access_mask.contains(AccessMask::FOOT));
    assert!(
        lift.forward_access_mask.contains(AccessMask::FOOT),
        "scenario-only forward direction must be physically materialized"
    );
    assert!(lift.reverse_access_mask.contains(AccessMask::FOOT));
    assert_eq!(
        lift.forward_extra_flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
        EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
        "only the direction absent from the source is marked temporal-only"
    );
    assert_eq!(lift.reverse_extra_flags, 0);
    assert!(
        scan.feature_attributes
            .value_matches(0, "feature_type", "staircase")
    );
    assert_eq!(scan.feature_attributes.row_count, 2);
    assert!(
        scan.feature_attributes
            .value(0, SOURCE_LAYER_SCHEMA_COLUMN)
            .is_some(),
        "each retained row records its source-layer schema"
    );
    assert_eq!(scan.temporal_rule_sets.len(), 1);
    assert_eq!(staircase.temporal_rule_id, Some(0));
}

// Mini-station fixture: an outdoor footway reaches a portal, where an
// escalator, stairs and a lift (from a second GeoPackage) climb 6 m to a
// concourse. A scheduled paid-area footway then leads to the platform and is
// the only connection to it.
const STREET: [f64; 3] = [114.0, 22.0, 0.0];
const PORTAL: [f64; 3] = [114.0001, 22.0, 0.0];
const CONCOURSE: [f64; 3] = [114.0001, 22.0, 6.0];
const GATE: [f64; 3] = [114.0002, 22.0, 6.0];
const PLATFORM: [f64; 3] = [114.0003, 22.0, 6.0];

const ESCALATOR: i64 = 1_000;
const STAIRS: i64 = 1_001;
const LIFT: i64 = 1_002;
const PAID_AREA: i64 = 1_003;

const STATION_PROFILE: &str = r#"
profile:
  id: mini_station_walk
  label: Mini-station walking
  mode: foot
  defaults_pack: test
cost:
  objective: generalized
  time_weight: 1.0
speed_rules:
  - match: {highway: footway}
    speed_kph: 4.8
  - match: {highway: steps}
    speed_kph: 3.0
slope_model:
  kind: piecewise
  points:
    - {gradient: -1.0, speed_factor: 0.4}
    - {gradient: 0.0, speed_factor: 1.0}
    - {gradient: 1.0, speed_factor: 0.3}
facilities:
  attribute: feature_type
  classes:
    stairs:
      kind: stairs
      vertical_speed_mps: 0.2
      boarding_penalty_s: 1.0
      burden_per_vertical_m: 2.0
      burden_component: stair_burden
    escalator:
      kind: escalator
      conveyor_speed_mps: 2.0
      walking_speed_mps: 0.5
      boarding_penalty_s: 1.0
    lift:
      kind: lift
      wait_s: 20.0
      vertical_speed_mps: 3.0
      wait_component: lift_wait
components:
  uncovered_time:
    expression: "travel_time * (covered == false)"
    weight: 0.0
  ascent_m:
    expression: ascent
    weight: 0.0
  stair_burden:
    expression: zero
    weight: 0.0
  lift_wait:
    expression: zero
    weight: 0.0
temporal:
  allow_wait: false
  max_wait_s: 0
returns:
  geometry: full
  segment_rows: true
"#;

/// The escalator runs downhill until 10:00 and uphill afterwards.
const DIRECTION_SCENARIO: &str = r#"id: mini_station_direction
features:
  - source_feature_id: 1000
    replace_rules: true
    rules:
      - day_mask: 255
        intervals: [{start_minute: 360, end_minute: 1440}]
        effect: open_only
      - day_mask: 255
        intervals: [{start_minute: 360, end_minute: 600}]
        effect: backward_only
      - day_mask: 255
        intervals: [{start_minute: 600, end_minute: 1440}]
        effect: forward_only
"#;

#[derive(Clone, Copy)]
struct StationFeature<'a> {
    id: i64,
    feature_type: &'a str,
    direction: i64,
    covered: bool,
    indoor_location: &'a str,
    wheelchair_barrier: bool,
    schedule: Option<&'a str>,
    polarity: Option<&'a str>,
    points: [[f64; 3]; 2],
}

/// Covered, two-way, public walkway; tests override the rest.
fn walkway<'a>(id: i64, feature_type: &'a str, points: [[f64; 3]; 2]) -> StationFeature<'a> {
    StationFeature {
        id,
        feature_type,
        direction: 0,
        covered: true,
        indoor_location: "public",
        wheelchair_barrier: false,
        schedule: None,
        polarity: None,
        points,
    }
}

struct MiniStation {
    dir: TempDir,
    topology: TopologyBundle,
    profile: ProfileDocument,
    metrics: CompiledProfileBundle,
}

impl MiniStation {
    fn build() -> Self {
        let dir = TempDir::new("mini-station");
        let outdoor_path = dir.join("outdoor.gpkg");
        let indoor_path = dir.join("indoor.gpkg");
        create_station_gpkg(
            &outdoor_path,
            &[StationFeature {
                covered: false,
                indoor_location: "outdoor",
                ..walkway(100, "footway", [STREET, PORTAL])
            }],
        );
        create_station_gpkg(
            &indoor_path,
            &[
                StationFeature {
                    direction: 1,
                    wheelchair_barrier: true,
                    ..walkway(ESCALATOR, "escalator", [PORTAL, CONCOURSE])
                },
                StationFeature {
                    wheelchair_barrier: true,
                    ..walkway(STAIRS, "stairs", [PORTAL, CONCOURSE])
                },
                walkway(LIFT, "lift", [PORTAL, CONCOURSE]),
                StationFeature {
                    indoor_location: "paid_area",
                    schedule: Some(r#"[{"day_code":"ED","from_time":600,"to_time":2359}]"#),
                    polarity: Some("OPEN"),
                    ..walkway(PAID_AREA, "footway", [CONCOURSE, GATE])
                },
                StationFeature {
                    covered: false,
                    indoor_location: "platform",
                    ..walkway(1_004, "footway", [GATE, PLATFORM])
                },
            ],
        );
        std::fs::write(dir.join("scenario.yml"), DIRECTION_SCENARIO).expect("scenario writes");

        let layer = |source: &str| {
            format!(
                r#"
  - source: "{source}"
    table: walkways
    geometry_column: geom
    defaults: {{highway: footway, access: [foot], direction: both}}
    fields:
      id: feature_id
      feature_type: feature_type
      direction:
        role: direction
        decode: {{"-1": reverse, "0": both, "1": forward}}
      covered:
        role: covered
        decode: {{"0": "false", "1": "true"}}
      indoor_location: indoor_location
      wheelchair_barrier:
        role: wheelchair_barrier
        decode: {{"0": "false", "1": "true"}}
      schedule: access_schedule
      polarity: access_schedule_polarity
"#
            )
        };
        let mapping: GeoPackageMapping = serde_yaml::from_str(&format!(
            "retain: all\nmaterialize_both_directions_for: [1000]\nlayers:\n{}{}",
            layer("outdoor.gpkg"),
            layer("indoor.gpkg"),
        ))
        .expect("mini-station mapping parses");
        let scan = scan_geopackages(&[outdoor_path.clone(), indoor_path], &mapping, &mut |_| {})
            .expect("both mini-station sources scan");
        assert_eq!(scan.pending_ways.len(), 6);
        assert_eq!(
            scan.node_coords.len(),
            5,
            "the coincident indoor/outdoor portal must be one 3D node"
        );
        assert_eq!(scan.temporal_rule_sets.len(), 1);

        let (topology, _, _) = crate::topology::build_topology_from_scan(
            &outdoor_path,
            "mini-station",
            scan,
            &mut |_| {},
        )
        .expect("mini-station topology builds");
        let profile: ProfileDocument =
            serde_yaml::from_str(STATION_PROFILE).expect("mini-station profile parses");
        let metrics = compile(&profile, &topology);
        Self {
            dir,
            topology,
            profile,
            metrics,
        }
    }

    fn edge_index(&self, feature_id: i64, direction: i8) -> usize {
        (0..self.topology.edge_count())
            .find(|&index| {
                let edge = self.topology.routing_edge(index);
                edge.source_way_id == feature_id && edge.source_direction == direction
            })
            .expect("station edge exists")
    }

    fn node_at(&self, [lon, lat, z]: [f64; 3]) -> u32 {
        self.topology
            .nodes
            .iter()
            .find(|node| node.lon == lon && node.lat == lat && node.z == z)
            .expect("station node exists")
            .node_id
            .0
    }

    fn feature_ids(&self, edge_path: &[u32]) -> Vec<i64> {
        edge_path
            .iter()
            .map(|&edge_id| {
                let index = (0..self.topology.edge_count())
                    .find(|&index| self.topology.routing_edge(index).edge_id.0 == edge_id)
                    .expect("path edge belongs to station topology");
                self.topology.routing_edge(index).source_way_id
            })
            .collect()
    }

    /// Routes under the escalator direction scenario at `departure_time`.
    fn route_at(
        &self,
        metrics: &CompiledProfileBundle,
        departure_time: &str,
        origin: [f64; 3],
        destination: [f64; 3],
    ) -> anyhow::Result<RouteResult> {
        let mut request = route_request(origin, destination);
        request.temporal = netweevil_query::TemporalRequestOptions {
            departure_time: Some(departure_time.to_string()),
            scenario: Some(
                netweevil_query::load_scenario_overlay(self.dir.join("scenario.yml"))
                    .expect("scenario loads"),
            ),
            ..Default::default()
        };
        netweevil_query::execute_route(&self.topology, metrics, &request)
    }
}

fn compile(profile: &ProfileDocument, topology: &TopologyBundle) -> CompiledProfileBundle {
    netweevil_profile::compile_profile_bundle(
        profile,
        topology,
        netweevil_core::CacheBundleId::new("mini-station"),
    )
    .expect("mini-station profile compiles")
}

/// Copy of `profile` that additionally excludes edges matching any of
/// the `attribute = value` pairs.
fn excluding(profile: &ProfileDocument, id: &str, rules: &[(&str, &str)]) -> ProfileDocument {
    let mut profile = profile.clone();
    profile.profile.id = id.to_string();
    for &(attribute, value) in rules {
        profile.exclude_rules.push(netweevil_profile::ExcludeRule {
            r#match: netweevil_profile::TagMatch {
                tags: [(attribute.to_string(), value.to_string())].into(),
            },
        });
    }
    profile
}

fn route_request(origin: [f64; 3], destination: [f64; 3]) -> RouteRequest {
    let point = |id: &str, [lon, lat, z]: [f64; 3]| netweevil_query::LabeledPoint {
        id: id.to_string(),
        lon,
        lat,
        z: Some(z),
    };
    RouteRequest {
        route_id: "mini-station".to_string(),
        origin: point("origin", origin),
        destination: point("destination", destination),
        snap: netweevil_query::SnapOptions {
            max_distance_m: 2.0,
            z_window_m: Some(0.25),
            attribute_filters: Default::default(),
            point_constraints: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: netweevil_profile::ReturnConfig {
            geometry: netweevil_profile::ReturnGeometry::Full,
            segment_rows: true,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

#[test]
fn mini_station_welds_levels_and_costs_vertical_facilities() {
    let station = MiniStation::build();
    let topology = &station.topology;
    assert_eq!(topology.nodes.len(), 5);
    assert!(topology.nodes.iter().any(|node| node.z == 0.0));
    assert!(topology.nodes.iter().any(|node| node.z == 6.0));

    let forward_escalator = station.edge_index(ESCALATOR, 1);
    let reverse_escalator = topology.routing_edge(station.edge_index(ESCALATOR, -1));
    assert_eq!(topology.routing_edge(forward_escalator).ascent_m, 6.0);
    assert_eq!(reverse_escalator.descent_m, 6.0);
    assert_eq!(
        reverse_escalator.flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
        EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
    );
    let paid_gate = topology.routing_edge(station.edge_index(PAID_AREA, 1));
    assert!(topology.feature_attributes.value_matches(
        paid_gate.feature_row,
        "indoor_location",
        "paid_area"
    ));

    let time = |edge: usize| station.metrics.edge_metrics[edge].travel_time_s.unwrap();
    let lift = station.edge_index(LIFT, 1);
    assert!(time(forward_escalator) < time(lift));
    assert!(time(lift) < time(station.edge_index(STAIRS, 1)));
    let lift_wait = station
        .metrics
        .components
        .iter()
        .find(|component| component.name == "lift_wait")
        .expect("lift wait component compiles");
    assert_eq!(lift_wait.edge_values[lift], 20.0);
}

#[test]
fn mini_station_routes_follow_the_escalator_direction_schedule() {
    let station = MiniStation::build();
    let metrics = &station.metrics;

    let uphill_after_flip = station
        .route_at(metrics, "2026-07-10T10:01:00+08:00", STREET, PLATFORM)
        .expect("uphill escalator route succeeds after the direction flip");
    assert_eq!(
        station.feature_ids(&uphill_after_flip.edge_path),
        vec![100, ESCALATOR, PAID_AREA, 1_004]
    );
    assert!(uphill_after_flip.summary.components["uncovered_time"] > 0.0);
    assert!(uphill_after_flip.summary.components["ascent_m"] >= 6.0);

    let uphill_before_flip = station
        .route_at(metrics, "2026-07-10T09:59:00+08:00", STREET, PLATFORM)
        .expect("lift remains available while the escalator runs downhill");
    assert_eq!(
        station.feature_ids(&uphill_before_flip.edge_path),
        vec![100, LIFT, PAID_AREA, 1_004]
    );

    let downhill_before_flip = station
        .route_at(metrics, "2026-07-10T09:59:00+08:00", PLATFORM, STREET)
        .expect("materialized downhill escalator route succeeds before the flip");
    assert_eq!(
        station.feature_ids(&downhill_before_flip.edge_path),
        vec![1_004, PAID_AREA, ESCALATOR, 100]
    );
}

#[test]
fn mini_station_paid_area_exclusion_disconnects_the_platform() {
    let station = MiniStation::build();
    let public_only = excluding(
        &station.profile,
        "mini_station_public_only",
        &[("indoor_location", "paid_area")],
    );
    let public_only_metrics = compile(&public_only, &station.topology);
    assert!(
        (0..station.topology.edge_count())
            .filter(|&index| station.topology.routing_edge(index).source_way_id == PAID_AREA)
            .all(|index| public_only_metrics.edge_metrics[index]
                .travel_time_s
                .is_none())
    );
    assert!(
        station
            .route_at(
                &public_only_metrics,
                "2026-07-10T10:01:00+08:00",
                STREET,
                PLATFORM
            )
            .is_err(),
        "the paid gate is the only connection to the platform"
    );
}

#[test]
fn mini_station_step_free_transfer_feeds_door_to_door_transit_routing() {
    let station = MiniStation::build();
    let step_free = excluding(
        &station.profile,
        "mini_station_step_free",
        &[("feature_type", "stairs"), ("feature_type", "escalator")],
    );
    let step_free_metrics = compile(&step_free, &station.topology);

    let gtfs_path = station.dir.join("gtfs");
    create_station_gtfs(&gtfs_path);
    let mut transit_bundle = netweevil_transit::import_gtfs(
        &gtfs_path,
        netweevil_transit::TransitImportOptions {
            name: "mini_station".to_string(),
            source_label: "mini_station".to_string(),
            service_start_date: "2026-07-10".to_string(),
            service_days: 1,
        },
    )
    .expect("mini-station GTFS imports");
    let bind = |stop_id: &str, position: [f64; 3]| netweevil_transit::TransitStopBinding {
        stop_id: stop_id.to_string(),
        target: netweevil_transit::TransitStopBindingTarget::Node {
            node_id: station.node_at(position),
        },
    };
    let binding_feed_id = transit_bundle.feed_id.clone();
    netweevil_transit::apply_transit_stop_bindings(
        &mut transit_bundle,
        &netweevil_transit::TransitStopBindingTable {
            schema_version: netweevil_transit::TRANSIT_STOP_BINDING_SCHEMA_VERSION,
            feed_id: Some(binding_feed_id),
            bindings: vec![bind("A", PORTAL), bind("B", PLATFORM)],
        },
    )
    .expect("platform and concourse stops bind to the 3D station graph");
    let transfer_table = netweevil_transit::build_transit_transfer_table(
        &transit_bundle,
        &netweevil_transit::TransitTransferBuildOptions {
            dataset_id: "mini-station".to_string(),
            profile_id: step_free.profile.id.clone(),
            profile_hash: step_free_metrics.profile_hash.clone(),
            max_transfer_distance_m: 100.0,
            max_candidates_per_stop: 4,
        },
        &StationTransferEstimator {
            topology: &station.topology,
            metrics: &step_free_metrics,
        },
    )
    .expect("profile-aware network transfer table builds");
    let network_transfer = transfer_table
        .transfers
        .iter()
        .find(|transfer| transfer.from_stop_id == "A" && transfer.to_stop_id == "B")
        .expect("concourse-to-platform network transfer exists");
    assert_eq!(
        station.feature_ids(&network_transfer.path.edge_path),
        vec![LIFT, PAID_AREA, 1_004]
    );
    assert_eq!(network_transfer.path.components["lift_wait"], 20.0);
    assert_eq!(network_transfer.path.geometry.first().unwrap()[2], 0.0);
    assert_eq!(network_transfer.path.geometry.last().unwrap()[2], 6.0);
    let expected_transfer_edge_path = network_transfer.path.edge_path.clone();

    let transit_router = netweevil_transit::PreparedTransitRouter::new_with_transfer_tables(
        std::sync::Arc::new(transit_bundle),
        vec![transfer_table],
    )
    .expect("CSA loads the mini-station transfer table");
    let point = |id: &str, lon: f64| netweevil_transit::TransitPoint {
        z: None,
        id: id.to_string(),
        lon,
        lat: 22.0,
    };
    let transit_result = transit_router
        .execute_route(&netweevil_transit::TransitRouteRequest {
            route_id: "mini-station-transit".to_string(),
            origin: point("origin", 113.9998),
            destination: point("destination", 114.0006),
            time: netweevil_transit::TransitQueryTime {
                datetime: "2026-07-10T07:59:00+08:00".to_string(),
                arrive_by: false,
                search_window_s: 3_600,
            },
            modes: netweevil_transit::TransitModeOptions {
                max_access_distance_m: 5.0,
                max_egress_distance_m: 5.0,
                max_transfer_distance_m: 100.0,
                transfer_profile_id: Some(step_free.profile.id.clone()),
                transfer_slack_s: 0,
                ..Default::default()
            },
            returns: netweevil_transit::TransitReturnOptions {
                include_geometry: true,
                walking_geometry: netweevil_transit::TransitWalkingGeometry::Network,
                ..Default::default()
            },
            alternatives: Default::default(),
        })
        .expect("door-to-door CSA route traverses the station transfer");
    assert_eq!(
        transit_result.outcome,
        netweevil_transit::TransitOutcome::Scheduled
    );
    let transfer_leg = transit_result
        .legs
        .iter()
        .find(|leg| leg.leg_type == netweevil_transit::TransitLegType::Transfer)
        .expect("CSA result exposes its in-station transfer leg");
    assert_eq!(
        transfer_leg
            .network_path
            .as_ref()
            .expect("station leg carries its graph path")
            .edge_path,
        expected_transfer_edge_path
    );
}

/// Street-time estimator that routes stop-to-stop transfers over the
/// station graph between the stops' bound nodes.
struct StationTransferEstimator<'a> {
    topology: &'a TopologyBundle,
    metrics: &'a CompiledProfileBundle,
}

impl netweevil_transit::StreetTimeEstimator for StationTransferEstimator<'_> {
    fn street_time_s(
        &self,
        _mode: netweevil_transit::AccessMode,
        _egress: bool,
        _from_lon: f64,
        _from_lat: f64,
        _to_lon: f64,
        _to_lat: f64,
    ) -> Option<u32> {
        None
    }

    fn stop_to_stop_path(
        &self,
        _mode: netweevil_transit::AccessMode,
        from: &netweevil_transit::TransitStop,
        to: &netweevil_transit::TransitStop,
    ) -> Option<netweevil_transit::TransitStreetPath> {
        let position = |stop: &netweevil_transit::TransitStop| {
            let netweevil_transit::TransitStopBindingTarget::Node { node_id } =
                stop.binding.as_ref()?
            else {
                return None;
            };
            let node = self.topology.nodes.get(*node_id as usize)?;
            Some([node.lon, node.lat, node.z])
        };
        let route = netweevil_query::execute_route(
            self.topology,
            self.metrics,
            &route_request(position(from)?, position(to)?),
        )
        .ok()?;
        Some(netweevil_transit::TransitStreetPath {
            travel_time_s: route.summary.total_travel_time_s.ceil() as u32,
            distance_m: Some(route.summary.total_distance_m as f64),
            edge_path: route.edge_path,
            geometry: route.geometry.unwrap_or_default(),
            components: route.summary.components,
        })
    }
}

fn create_station_gpkg(path: &Path, features: &[StationFeature<'_>]) {
    let connection = Connection::open(path).expect("station GeoPackage opens");
    connection
        .execute_batch(&format!(
            "{GEOMETRY_COLUMNS_DDL}
             CREATE TABLE walkways (
                id INTEGER PRIMARY KEY,
                feature_type TEXT NOT NULL,
                direction INTEGER NOT NULL,
                covered INTEGER NOT NULL,
                indoor_location TEXT NOT NULL,
                wheelchair_barrier INTEGER NOT NULL,
                schedule TEXT,
                polarity TEXT,
                geom BLOB NOT NULL
             );"
        ))
        .expect("station GeoPackage schema");
    for feature in features {
        connection
            .execute(
                "INSERT INTO walkways VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    feature.id,
                    feature.feature_type,
                    feature.direction,
                    i64::from(feature.covered),
                    feature.indoor_location,
                    i64::from(feature.wheelchair_barrier),
                    feature.schedule,
                    feature.polarity,
                    gpkg_linestring_z(&feature.points),
                ],
            )
            .expect("station feature inserts");
    }
}

/// Two Friday trips: origin -> concourse stop A, then platform stop B ->
/// destination, so the only itinerary transfers A -> B inside the station.
fn create_station_gtfs(path: &Path) {
    std::fs::create_dir_all(path).expect("mini-station GTFS directory creates");
    for (name, contents) in [
        (
            "agency.txt",
            "agency_id,agency_name,agency_url,agency_timezone\nMINI,Mini station,https://example.test,Asia/Hong_Kong\n",
        ),
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nO,Origin,22.0,113.9998\nA,Concourse,22.0,114.0001\nB,Platform,22.0,114.0003\nD,Destination,22.0,114.0006\n",
        ),
        (
            "routes.txt",
            "route_id,route_short_name,route_long_name,route_type\nR1,1,Inbound,3\nR2,2,Outbound,3\n",
        ),
        (
            "trips.txt",
            "route_id,service_id,trip_id,trip_headsign\nR1,FRIDAY,T1,Concourse\nR2,FRIDAY,T2,Destination\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:00:00,08:00:00,O,1\nT1,08:02:00,08:02:00,A,2\nT2,08:05:00,08:05:00,B,1\nT2,08:07:00,08:07:00,D,2\n",
        ),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nFRIDAY,0,0,0,0,1,0,0,20260710,20260710\n",
        ),
    ] {
        std::fs::write(path.join(name), contents).expect("mini-station GTFS file writes");
    }
}
