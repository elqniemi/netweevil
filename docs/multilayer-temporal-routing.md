# Multilayer, temporal, and transit analysis

NetWeevil can import a lossless 3D pedestrian network from one or more
GeoPackages, compile slope- and facility-aware profiles, evaluate runtime
closures and continuous overlays, bind GTFS stops to exact graph locations,
and run criticality, constrained-route, Pareto, and scenario-diff analyses.
The engine remains geography-neutral: source meanings live in a mapping file,
and changing conditions live in scenario, calendar, and overlay files.

The files under `examples/` are executable schema examples. Values and source
feature ids whose names start with `hk_example`, `illustrative`, or `DEMO` are
Hong Kong-shaped examples only. They are not built-in network behavior and
must be replaced with ids and coordinates from the analysis dataset.

## Preprocess the source data

GeoPackage ingest is deliberately projection-free. Every input must already
contain longitude/latitude in WGS84 order and a Z coordinate in metres in one
consistent vertical datum. NetWeevil does not read a raw Hong Kong 1980 Grid
network in EPSG:2326, does not transform HKPD heights from EPSG:5738, and does
not read a FileGDB directly.

For the Hong Kong analysis, the preprocessing pipeline must therefore:

1. export the outdoor and indoor FileGDB layers to GeoPackage;
2. transform horizontal coordinates from EPSG:2326 to WGS84 longitude and
   latitude;
3. preserve or derive Z in metres in the chosen vertical datum (HKPD for the
   project) and use that datum consistently in every source;
4. retain every source vertex and field; and
5. make feature-id namespaces disjoint across the files.

The repository includes a validated GDAL workflow for the supplied outdoor and
indoor GeoPackages:

```bash
scripts/prepare_hong_kong_pedestrian.sh \
  /path/to/hong-kong-analysis
```

It reads the two files under `datasets/`, writes the same basenames and layer
names under `prepared/`, transforms only the EPSG:2326 horizontal coordinates
to WGS84 longitude/latitude, and preserves the third coordinate as EPSG:5738
HKPD metres. Feature counts, field names/types/domains, geographic bounds, and
a sampled Z value are checked before each output is installed atomically. A
source-hash and GDAL provenance manifest is written alongside the outputs. Use
`--output-dir DIR` for another destination, `--check-only` for validation, and
`--force` for an intentional replacement.

Do not pass projected eastings and northings directly to `dataset import`: the
importer expects geographic coordinate ranges and does no hidden reprojection.

Portals weld only when the post-transformation `(lon, lat, z)` coordinates
quantize to the same values. The default example tolerances are `1e-9` degrees
and `0.01` m. Preserve coincident indoor/outdoor portal coordinates through the
same deterministic transformation rather than snapping them independently.

## Map and import GeoPackages

[`examples/ingest/multilayer_walkways_mapping.yml`](../examples/ingest/multilayer_walkways_mapping.yml)
shows a generic version-1 mapping schema. The HK-specific
[`hong_kong_pedestrian_mapping.yml`](../examples/ingest/hong_kong_pedestrian_mapping.yml)
uses the inspected source schemas: `pedestrian_route` and
`indoor_pedestrian_route`, geometry column `Shape`, and
`PedestrianRouteID` as the feature id. It maps `FeatureType`, `WeatherProof`,
`Location`, `Enabled`, wheelchair fields, `access_time_details`,
`access_time_type`, and the source grouping/name columns while retaining
everything else.

Each mapped layer needs a `feature_id` role. Other useful roles include
`direction`, `class` or `feature_type`,
`covered`, `indoor_location`, `enabled`, `wheelchair_access`, `wheelchair_barrier`,
`access_schedule`, a schedule-polarity role, names, and grouping ids. Profile
rules can address either an original source column name or its semantic role.

`retain: all` is required. All non-geometry source fields are stored in the
typed feature-attribute table; coded-value domains from the GeoPackage are
retained, and mapping-level `decode` entries can add or override values used by
matching. Source polylines are split at every vertex, and every derived edge
keeps its source feature row and source feature id.

The importer also retains reserved compact metadata recording part boundaries,
the positions of consecutive vertices that collapse under coordinate
quantization, sparse exact-coordinate overrides where a welded vertex differs
from its canonical node, edge-less part anchors, and each row's source-layer
schema. Together with source-oriented edge order, this preserves
MultiLineString structure and repeated/raw vertices without duplicating the
ordinary geometry buffer, and detects a field that disappears from only one
input layer.

Direction values decode to `forward`, `reverse`, or `both`. If a runtime
scenario may reverse a one-way feature, list its source id under
`materialize_both_directions_for` before import so both directed edges exist.

An `access_schedule` source cell is a JSON array. For example:

```json
[
  {"day_code":"WD","from_time":600,"to_time":2359},
  {"day_code":"PH","from_time":"10:00","to_time":"18:00"}
]
```

Supported day codes include `ED`, individual weekdays, `WD`, `WE`, and `PH`.
`OPEN` polarity means the feature is closed outside the listed intervals;
`CLOSE` means it is closed inside them. `2400` and `2359` are accepted as the
end of day, and an end earlier than the start wraps across midnight.

The published HK files are named `3D_Pedestrian_Network.gpkg` and
`3D_Indoor_Network.gpkg`. Preserve those basenames, layer/field names, coded
domains, and vertices in the prepared outputs so the supplied mapping and
lossless audit describe the source faithfully. In particular, the real coded
values include `FeatureType` facilities and their `M_` variants (canonicalized
by the mapping so both variants match the same profile class),
`WeatherProof` 1/2/3 as covered/noncovered/unclassified, `Location` 1/2 plus
indoor value 3 for paid area, wheelchair booleans encoded as 1/2, and
`Direction` -1/0/1.

Import the prepared outdoor and indoor sources into one topology:

```bash
cargo run -p netweevil-cli -- dataset import \
  /path/to/hong-kong-analysis/prepared/3D_Pedestrian_Network.gpkg \
  /path/to/hong-kong-analysis/prepared/3D_Indoor_Network.gpkg \
  --name hk_pedestrian_3d \
  --format gpkg \
  --mapping examples/ingest/hong_kong_pedestrian_mapping.yml
```

The `source:` selectors in the mapping must match the actual input file names
or path suffixes. Duplicate feature ids across sources are rejected. Import
progress reports feature, segment-chain, and welded-node counts.

Run the lossless round-trip audit against the same source files and mapping:

```bash
cargo run -p netweevil-cli -- dataset audit \
  --dataset hk_pedestrian_3d \
  --against \
    /path/to/hong-kong-analysis/prepared/3D_Pedestrian_Network.gpkg \
    /path/to/hong-kong-analysis/prepared/3D_Indoor_Network.gpkg \
  --mapping examples/ingest/hong_kong_pedestrian_mapping.yml \
  --json
```

The audit re-reads the sources and fails on missing or unexpected features or
fields, type/semantic-role/domain drift, retained-value drift, source-oriented
vertex/part-order drift, and missing or unexpected quantized 3D segments. It is
not a replacement for project-specific portal QA. The Hong Kong preprocessing
pipeline should separately count indoor/outdoor shared portal vertices and
compare that count with its audited expectation (roughly 900 in the source
plan); that number is a dataset check, not an engine constant.

## Compile pedestrian profiles

Two complete profiles are provided:

- [`pedestrian_multilayer.yml`](../examples/profiles/pedestrian_multilayer.yml)
  uses Tobler slope adjustment, stairs, escalators, lifts, travelators and
  ramps, and named exposure/ascent/facility components.
- [`pedestrian_step_free_multilayer.yml`](../examples/profiles/pedestrian_step_free_multilayer.yml)
  excludes barriers, stairs and escalators, uses a piecewise slope model, and
  prices lift and ramp traversal.

Components are accumulated alongside the scalar generalized cost. An
expression has one base quantity (`travel_time`, `distance`, `ascent`,
`descent`, or `zero`), optional attribute predicates, and at most one temporal
overlay term. For example:

```yaml
components:
  uncovered_time:
    expression: "travel_time * (covered == false)"
    weight: 0.25
  sun_time:
    expression: "travel_time * (covered == false) * (1 - overlay:shade_fraction)"
    weight: 0.50
```

Component weights contribute to the scalar objective. A zero weight records a
quantity without changing route choice. Facility-generated components such as
`lift_wait` can use an expression of `zero`; the facility model supplies their
edge values.

Validate and compile against the imported attribute registry:

```bash
cargo run -p netweevil-cli -- profile validate \
  examples/profiles/pedestrian_multilayer.yml

cargo run -p netweevil-cli -- profile compile \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml
```

Validation checks the profile document. Compilation additionally rejects a
matcher, facility attribute, or component predicate that is absent from the
dataset's original-column/semantic-role registry.

Both supplied profiles exclude `enabled=false`. The HK `WheelchairAccess`
field identifies an accessible entrance; it is retained for reporting and
entrance selection but is not a general edge-passability flag. The step-free
profile therefore excludes `wheelchair_barrier=true` plus stair/escalator
facilities, rather than deleting every ordinary link whose
`wheelchair_access` value is false.

The supplied profiles allow paid-area links so they can support platform and
door-to-door transit work. For a street-only analysis that must not cross a
fare gate, add an explicit semantic-attribute exclusion:

```yaml
exclude_rules:
  - match:
      indoor_location: paid_area
```

## Temporal requests, scenarios, and overlays

Route, OD, matrix, service-area, accessibility, and betweenness documents can
carry flattened temporal fields:

- `departure_time` or `arrive_by`: RFC 3339 with an explicit offset;
- `scenario`: one YAML or JSON feature-state overlay;
- `holiday_calendar`: a YAML or JSON date list;
- `overlay`: a list of continuous CSV or Parquet series;
- `max_labels_per_state` and `arrive_by_lookback_s`: exact-search guards.

The corresponding street-analysis CLI commands (`route`, `route-batch`, `od`,
`matrix`, `accessibility`, `service-area`, `service-area-sequence`, and
`betweenness`) also accept `--departure-time`, `--scenario`,
`--holiday-calendar`, and repeatable `--overlay` flags. Optional scalar flags
replace only their matching request-file value; unspecified fields remain
unchanged. CLI overlays are appended after request-file overlays, preserving
both sets and giving later CLI entries their normal overlay precedence. An
explicit `--departure-time` replaces an `arrive_by` anchor because the two
forms are mutually exclusive. Every CLI run manifest stores the merged,
structured request under `effective_request`, so these overrides remain
reproducible without modifying the source request file. For example:

```bash
cargo run -p netweevil-cli -- analyze route \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_temporal_route.json \
  --departure-time 2026-07-10T09:55:00+08:00 \
  --scenario examples/scenarios/hong_kong_central_escalator_example.yml \
  --holiday-calendar examples/temporal/example_holidays.yml \
  --overlay examples/temporal/shade_fraction.csv
```

Matrix overrides follow the point-set side that already carries exact temporal
options (origins by default); identical options are applied to both when both
sides are active. On `service-area-sequence`, `--departure-time` intentionally
replaces the range/list with one explicit frame, while the other flags override
the nested service-area template.

Scenarios and continuous overlays require `departure_time` or `arrive_by`.
Temporal searches evaluate each rule and overlay at edge-entry time and use the
exact label-setting engine; ordinary static requests remain eligible for CCH.
Waiting at a closed edge is allowed only when the compiled profile sets
`temporal.allow_wait: true`, and is capped by `max_wait_s`.
Time-varying `SpeedFactor` rules require that positive wait policy: the exact
engine takes the earliest-exit lower envelope across the current and future
minute regimes, including a bounded voluntary wait when it arrives sooner.
If an overlay-backed component is nonzero on the same speed-varying feature
and the loaded series defines that component there, the request fails clearly
because fastest exit and lowest exposure can be nondominated traversal choices;
model those alternatives explicitly instead of returning a heuristic frontier.

A scenario addresses source features, not internal edge ids:

```yaml
id: maintenance
features:
  - source_feature_id: 900002
    force_closed: true
  - source_feature_id: 900003
    speed_factor: 0.5
```

It may also set `force_open`, replace imported rules, or add rules with effects
`closed`, `open_only`, `forward_only`, `backward_only`, or a serialized
`speed_factor`. Rule intervals use inclusive `start_minute` and exclusive
`end_minute`; a start after the end wraps midnight. Day-mask bits are Monday
through Sunday (`1` through `64`) plus public holiday (`128`). On a date listed
in the holiday calendar, the PH bit replaces the weekday bit, so use mask `255`
when a rule must cover weekdays and holidays.

[`hong_kong_central_escalator_example.yml`](../examples/scenarios/hong_kong_central_escalator_example.yml)
uses audited `PedestrianRouteID=250009423`, whose geometry is digitized uphill,
as a representative runtime direction override. All 33 related source features
selected for bidirectional materialization—and their source `Direction` and
`access_time_details` values—are preserved in
[`hong_kong_central_mid_levels_source_features.csv`](../examples/scenarios/hong_kong_central_mid_levels_source_features.csv).
That inventory reflects the source snapshot inspected in July 2026. The
snapshot has separate scheduled-direction features, so imported schedules may
already encode the operating pattern; the scenario deliberately demonstrates
replacement at runtime. Re-audit ids, geometry direction, and schedules after
any source refresh.

The HK mapping decodes `WeatherProof=3` as `covered=unknown`. The supplied
profiles report that time separately as `coverage_unknown_time` with zero
weight, allowing the analysis repository to publish covered/uncovered
sensitivity bounds rather than silently assigning the unclassified links.

Continuous overlays have required `feature_id`, `t_start`, and `t_end` columns;
every other non-empty column is a numeric value available as
`overlay:<column>`. Times are RFC 3339 and intervals are start-inclusive,
end-exclusive. See
[`shade_fraction.csv`](../examples/temporal/shade_fraction.csv). NetWeevil
consumes externally computed shade or heat series; it does not compute solar
geometry.

Run the example after adapting coordinates and feature ids:

```bash
cargo run -p netweevil-cli -- analyze route \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_temporal_route.json \
  --out .netweevil/runs/hk-example-temporal-route.geojson
```

The example point Z values and `snap.z_window_m` prevent snapping across
stacked levels. `snap.attribute_filters` can further restrict candidates to a
semantic/source attribute value.

### Temporal service-area sequences

A service-area sequence replays one spatial request across either an explicit
`departure_times` list or an inclusive `start_time`/`end_time` range with
`step_s`. The nested request must omit its own `departure_time` and `arrive_by`;
the sequence supplies one departure time per frame. Do not provide both timing
forms. Sequences are capped at 10,000 frames.

[`hong_kong_example_service_area_sequence.json`](../examples/requests/hong_kong_example_service_area_sequence.json)
runs nine five-minute frames from 09:40 through 10:20, spanning the example
10:00 escalator direction change:

```bash
cargo run -p netweevil-cli -- analyze service-area-sequence \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_service_area_sequence.json \
  --out .netweevil/runs/hk-example-ten-am-frames.geojson
```

Sequence output supports JSON and GeoJSON. GeoJSON is one FeatureCollection;
each feature carries `sequence_id`, `frame_index`, `departure_time`,
`scenario_id`, and `analysis_id`, and collection metadata lists the ordered
departure times. This is directly usable as a temporal animation layer.

The API accepts the same request at `POST /v1/service-area-sequence`. Use
`?format=geojson` for the flattened animated FeatureCollection:

```bash
curl -X POST \
  'http://127.0.0.1:8080/v1/service-area-sequence?format=geojson' \
  -H 'content-type: application/json' \
  --data-binary @- <<'JSON'
{
  "profile_id": "pedestrian_multilayer",
  "request": {
    "sequence_id": "boundary_frames",
    "request": {
      "analysis_id": "boundary_area",
      "origins": [{"id":"origin","lon":114.1517,"lat":22.2839,"z":34.0}],
      "thresholds": [{"id":"ten_minutes","limit":600,"metric":"travel_time_s"}],
      "scenario": "examples/scenarios/hong_kong_central_escalator_example.yml"
    },
    "start_time": "2026-07-10T09:40:00+08:00",
    "end_time": "2026-07-10T10:20:00+08:00",
    "step_s": 300
  }
}
JSON
```

### Service-area component vectors

When a compiled profile declares named components, each service-area segment
and corresponding network feature reports `start_components` and
`end_components`. These are cumulative totals on the winning origin-to-segment
path at the start and end fractions, not merely the component contribution of
that segment. Static component-aware service areas intentionally use exact,
threshold-bounded Dijkstra instead of PHAST: PHAST retains scalar labels and
cannot reconstruct the additive component vector. The result warning records
that tradeoff.

With more than one origin, `multi_origin_mode: merge` unions reachable spans.
A merged span has no single owning origin or winning path, so its cumulative
component maps are deliberately empty and the result carries a warning. Use
`multi_origin_mode: overlap` to keep each origin's overlapping paths, or `cut`
to assign disjoint spans while retaining path-owned component labels. Polygon
and other aggregate summaries likewise do not invent a single cumulative
vector for a union; consume the network segments/features when path-level
component accounting matters.

## Component budgets and Pareto routes

A route request may add hard component budgets and a two-objective frontier:

```json
{
  "constraints": [{"component":"uncovered_time","max_value":300.0}],
  "pareto": {
    "component":"uncovered_time",
    "max_routes":8,
    "max_labels_per_state":96
  }
}
```

Run the full fixture with:

```bash
cargo run -p netweevil-cli -- analyze route \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_pareto_route.json \
  --out .netweevil/runs/hk-example-pareto.geojson
```

Component constraints and Pareto routing use exact nondominated label-setting.
They work for static routes and for temporal `departure_time` or `arrive_by`
requests, including scenario rules and continuous component overlays evaluated
at edge-entry time. They still require the strict fallback policy. Per-state
label and returned-route caps bound worst-case growth.

## Betweenness and scenario batches

Demand-weighted edge betweenness multiplies each origin weight by each
destination weight and adds that demand to every traversed directed edge. It
can run statically or with temporal fields:

```bash
cargo run -p netweevil-cli -- analyze betweenness \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_betweenness.json \
  --out .netweevil/runs/hk-example-betweenness.gpkg
```

Outputs may be JSON, CSV, GeoJSON, or GeoPackage. Scores are stored per
directed edge with source feature id, routed-pair count, raw score, normalized
score, and 3D geometry.

A scenario batch runs a baseline and then resolves one scenario selector for
every case while retaining each embedded request's calendar and continuous
overlays:

```bash
cargo run -p netweevil-cli -- analyze scenario-batch \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_scenario_batch.yml \
  --out .netweevil/runs/hk-example-scenario-batch.json
```

The document accepts embedded `routes`, `service_areas`, `accessibility`, `od`,
`matrices`, and `betweenness` analyses. OD entries wrap an `OdPairsDocument` as
`{analysis_id, request}`; matrix entries use
`{analysis_id, origins, destinations}`; betweenness entries use the ordinary
`BetweennessRequest` shape. The supplied fixture exercises routes, OD, matrix,
and weighted betweenness under both scenario cases.

Each scenario must select exactly one source: an explicit `overlay`, a `top_k`
ranking, or a retained-attribute `group`. Explicit overlays are parsed and every
referenced source feature id is checked against the active topology before the
baseline runs; ranked source feature ids receive the same check. Missing files
and selectors produced for an older topology therefore fail clearly instead of
creating an ineffective closure.
For example:

```yaml
scenarios:
  - id: explicit_failure
    overlay: failures/lift.yml
  - id: top_10_critical
    top_k:
      ranking: outputs/betweenness.json
      count: 10
  - id: close_one_mall
    group:
      attribute: building_id_1
      value: "1234"
```

`top_k.ranking` accepts CSV, JSON, YAML, or a prior betweenness-result JSON.
Generated `top_k` and `group` cases return their resolved
`closed_source_feature_ids` so the exact closure set remains auditable.

Its per-scenario diff reports disconnected route/accessibility demand,
disconnected OD pairs/matrix cells/betweenness pairs, signed and positive
rerouting burden, weighted disconnected demand, edge-usage score change,
service-area network loss, and reachable-destination loss. Betweenness losses
are matched by baseline origin/destination indexes, so newly reachable demand
cannot hide a different pair that became disconnected. Scenario snapshots
include these compact pair outcomes; ordinary betweenness edge exports omit
them. Paths inside a scenario-batch document—including temporal paths in OD,
matrix, and betweenness inputs—are resolved relative to that document; paths in
ordinary route or betweenness fixtures are interpreted from the process
working directory.

## Bind transit stops and build network transfers

A feed-scoped binding table can target a graph node, an edge plus optional
fraction, or a coordinate with Z and an attribute filter. JSON and CSV examples
are in [`examples/transit`](../examples/transit). The included Hong Kong stop
ids, node ids, edge ids, and coordinates are illustrative and must be replaced.

Apply bindings while importing GTFS or later:

```bash
cargo run -p netweevil-cli -- transit import path/to/feed.zip \
  --name hk_example_mtr \
  --service-start 2026-07-10 \
  --service-days 7 \
  --stop-bindings examples/transit/hong_kong_example_stop_bindings.json

cargo run -p netweevil-cli -- transit bindings apply \
  --feed hk_example_mtr \
  examples/transit/hong_kong_example_stop_bindings.json
```

Build a directed, profile-specific stop-to-stop table through the street and
indoor graph:

```bash
cargo run -p netweevil-cli -- transit transfers build \
  --feed hk_example_mtr \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_step_free_multilayer.yml \
  --max-transfer-distance-m 500 \
  --max-candidates-per-stop 32
```

Select it with
`modes.transfer_profile_id: "pedestrian_step_free_multilayer"` in a transit
request. When selected,
missing directed transfers remain unreachable rather than falling back to
straight-line walking. Returned access, transfer, and egress legs may expose
their edge path, named components, and 3D geometry. See
[`transit-fusion.md`](transit-fusion.md) for the result and host-integration
contract.

GTFS import accepts a UTF-8 BOM, times after 24:00, and interpolates blank
intermediate stop times when timed endpoints exist. The Hong Kong heavy/light
rail schedule and any feed repair remain preprocessing artifacts; NetWeevil
does not embed a Hong Kong timetable.

The [full Hong Kong tutorial](hong-kong-tutorial.md) now prepares numbered MTR
platform bindings from the separate CSDI-derived catalogue, combines official
surface services with community MTR/light rail, and audits source connectivity.
Community rail times remain estimates. The older `ILLUSTRATIVE-*` fixture only
demonstrates the accepted schema; the full example generates real bindings
under `.netweevil/hong-kong/transit/`.
