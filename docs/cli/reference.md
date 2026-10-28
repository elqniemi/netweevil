# CLI reference

This page contains the `--help` output of each `netweevil` command.

!!! note "Generated page"
    Do not edit this page. The script `scripts/gen_cli_reference.sh` writes
    it from the built binary. To regenerate the page, run the script from the
    repository root:

    ```bash
    scripts/gen_cli_reference.sh
    ```

## `netweevil` { #netweevil }

```text
Analyze OSM road networks, GTFS transit and traffic on your computer

Usage: netweevil <COMMAND>

Commands:
  dataset     Import, update, audit and list road network datasets
  transit     Import GTFS feeds and prepare them for transit routing
  traffic     Import historical speeds and route with traffic
  profile     Validate routing profiles and compile them for a dataset
  analyze     Run routing analyses and save each result as a run
  experiment  Run reproducible study files
  simulate    Validate and run agent-based traffic simulations
  report      Render reports from run manifests
  cache       List and prune compiled profiles and bundle files
  bench       Measure routing speed and quality
  api         Run the HTTP API server
  bootstrap   Start the API and web console in setup mode for a new workspace
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### `netweevil dataset` { #netweevil-dataset }

```text
Import, update, audit and list road network datasets

Usage: netweevil dataset <COMMAND>

Commands:
  import  Import a road network from OSM, Overture or GeoPackage sources
  update  Apply OSM change files or a newer extract to an imported OSM dataset
  audit   Compare a GeoPackage dataset with its source files
  list    List the imported datasets
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil dataset import` { #netweevil-dataset-import }

```text
Import a road network from OSM, Overture or GeoPackage sources

Usage: netweevil dataset import [OPTIONS] --name <NAME> <SOURCES>...

Arguments:
  <SOURCES>...  OSM `.osm.pbf` extract, an Overture transportation GeoParquet file, or a directory of Overture parquet files

Options:
      --name <NAME>
          Id of the new dataset
      --format <FORMAT>
          Source format; detected from the path when omitted [possible values: osm-pbf, overture, gpkg]
      --mapping <MAPPING>
          GeoPackage field/semantic mapping (YAML or JSON)
      --admin-boundaries <ADMIN_BOUNDARIES>
          Country (admin_level 2) and state (admin_level 4) boundaries as GeoJSON or OSM PBF, used instead of the boundary relations in an OSM extract (for extracts that cut them)
      --dem <DEM>
          DEM (ESRI ASCII grid `.asc` or GeoTIFF) sampled for node heights and edge grades of OSM and Overture sources
      --dem-crs <DEM_CRS>
          DEM CRS as `EPSG:<code>` or a `+proj=` definition; GeoTIFFs normally carry their own, ASCII grids default to longitude/latitude
      --dem-vertical-datum <DEM_VERTICAL_DATUM>
          Vertical datum label recorded with DEM heights (for example `HKPD`)
  -h, --help
          Print help
```

#### `netweevil dataset update` { #netweevil-dataset-update }

```text
Apply OSM change files or a newer extract to an imported OSM dataset.

The update reuses the CCH elimination order of the dataset.

Usage: netweevil dataset update [OPTIONS] --dataset <DATASET> <--diff <DIFF>...|--source <SOURCE>>

Options:
      --dataset <DATASET>
          Id of an imported OSM dataset

      --diff <DIFF>...
          osmChange file (`.osc` or `.osc.gz`); repeat to apply several in order

      --source <SOURCE>
          A newer `.osm.pbf` extract of the same area, used instead of change files

      --reorder <REORDER>
          `auto` patches the previous CCH order and runs a full nested dissection when the shortcut fill grows past `--max-fill-growth`; `always` reorders from scratch; `never` always patches
          
          [default: auto]
          [possible values: auto, always, never]

      --max-fill-growth <MAX_FILL_GROWTH>
          Shortcuts per edge state relative to the last full ordering above which `--reorder auto` reorders
          
          [default: 1.1]

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil dataset audit` { #netweevil-dataset-audit }

```text
Compare a GeoPackage dataset with its source files

Usage: netweevil dataset audit [OPTIONS] --dataset <DATASET> --against <AGAINST>... --mapping <MAPPING>

Options:
      --dataset <DATASET>     Id of an imported GeoPackage dataset
      --against <AGAINST>...  Original GeoPackage source(s) to compare against the retained bundle
      --mapping <MAPPING>     GeoPackage field/semantic mapping (YAML or JSON) used at import
      --json                  Emit the full machine-readable audit report
  -h, --help                  Print help
```

#### `netweevil dataset list` { #netweevil-dataset-list }

```text
List the imported datasets

Usage: netweevil dataset list

Options:
  -h, --help  Print help
```

### `netweevil transit` { #netweevil-transit }

```text
Import GTFS feeds and prepare them for transit routing

Usage: netweevil transit <COMMAND>

Commands:
  import     Import a GTFS feed for a window of service days
  bindings   Bind transit stops to exact network nodes or edges
  transfers  Build stop-to-stop transfer tables over the street network
  list       List the imported transit feeds
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil transit import` { #netweevil-transit-import }

```text
Import a GTFS feed for a window of service days

Usage: netweevil transit import [OPTIONS] --name <NAME> --service-start <SERVICE_START> <SOURCE>

Arguments:
  <SOURCE>  GTFS zip archive or directory of GTFS text files

Options:
      --name <NAME>                    Id of the new transit feed
      --service-start <SERVICE_START>  First service date to import (YYYY-MM-DD)
      --service-days <SERVICE_DAYS>    Number of service days to import from the first date [default: 7]
      --stop-bindings <STOP_BINDINGS>  Optional JSON/CSV stop binding table applied before the bundle is written. The same operation is available later through `transit bindings apply`
  -h, --help                           Print help
```

#### `netweevil transit bindings` { #netweevil-transit-bindings }

```text
Bind transit stops to exact network nodes or edges

Usage: netweevil transit bindings <COMMAND>

Commands:
  apply  Apply a stop binding table to an imported feed
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

##### `netweevil transit bindings apply` { #netweevil-transit-bindings-apply }

```text
Apply a stop binding table to an imported feed.

The table replaces all current bindings of the feed.

Usage: netweevil transit bindings apply --feed <FEED> <BINDINGS>

Arguments:
  <BINDINGS>
          JSON or CSV binding table

Options:
      --feed <FEED>
          Imported transit feed identifier

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil transit transfers` { #netweevil-transit-transfers }

```text
Build stop-to-stop transfer tables over the street network

Usage: netweevil transit transfers <COMMAND>

Commands:
  build  Route transfers between nearby stops with a foot profile
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

##### `netweevil transit transfers build` { #netweevil-transit-transfers-build }

```text
Route transfers between nearby stops with a foot profile.

The command registers the transfer table on the feed.

Usage: netweevil transit transfers build [OPTIONS] --feed <FEED> --dataset <DATASET> --profile <PROFILE>

Options:
      --feed <FEED>
          Imported transit feed identifier

      --dataset <DATASET>
          Street/indoor dataset containing the bound stops

      --profile <PROFILE>
          Compiled foot profile used to route station transfers

      --max-transfer-distance-m <MAX_TRANSFER_DISTANCE_M>
          Maximum straight-line stop separation considered for transfer routing
          
          [default: 500]

      --max-candidates-per-stop <MAX_CANDIDATES_PER_STOP>
          Maximum nearest candidate stops routed from each stop
          
          [default: 32]

      --out <OUT>
          Optional output path. `.json` produces readable JSON; other extensions use the packed binary layout. The generated table is registered on the feed

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil transit list` { #netweevil-transit-list }

```text
List the imported transit feeds

Usage: netweevil transit list

Options:
  -h, --help  Print help
```

### `netweevil traffic` { #netweevil-traffic }

```text
Import historical speeds and route with traffic

Usage: netweevil traffic <COMMAND>

Commands:
  import      Import a historical speed source for a dataset
  list        List the imported traffic sets
  synthesize  Write a deterministic synthetic speed source for a dataset's roads
  route       Route with historical and live traffic
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil traffic import` { #netweevil-traffic-import }

```text
Import a historical speed source for a dataset.

The source is a YAML or JSON document with optional CSV files.

Usage: netweevil traffic import [OPTIONS] --dataset <DATASET> <SOURCE>

Arguments:
  <SOURCE>
          Traffic source document (YAML or JSON)

Options:
      --dataset <DATASET>
          Id of an imported dataset

      --name <NAME>
          Traffic set id; defaults to the source's `traffic_id`

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil traffic list` { #netweevil-traffic-list }

```text
List the imported traffic sets

Usage: netweevil traffic list

Options:
  -h, --help  Print help
```

#### `netweevil traffic synthesize` { #netweevil-traffic-synthesize }

```text
Write a deterministic synthetic speed source for a dataset's roads

Usage: netweevil traffic synthesize [OPTIONS] --dataset <DATASET> --name <NAME> --out <OUT>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --name <NAME>
          Traffic set id written into the source
      --out <OUT>
          Output document (`.yml`); assignments go to `<stem>_assignments.csv`
      --timezone <TIMEZONE>
          IANA time zone of the weekly speed patterns [default: Europe/Amsterdam]
      --bucket-minutes <BUCKET_MINUTES>
          Length of one speed bucket in minutes [default: 15]
      --classes <CLASSES>
          Road classes that get speed patterns, separated by commas [default: motorway trunk primary secondary] [possible values: motorway, trunk, primary, secondary, tertiary, residential, service]
      --variants <VARIANTS>
          Speed-scale variants per road class [default: 4]
  -h, --help
          Print help
```

#### `netweevil traffic route` { #netweevil-traffic-route }

```text
Route with historical and live traffic

Usage: netweevil traffic route [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML) of a motorized mode
      --traffic <TRAFFIC>
          Imported historical traffic set; omit for live updates only
      --request <REQUEST>
          Route request (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          RFC 3339 departure; replaces the request's departure_time/arrive_by
      --mode <MODE>
          Traffic mode; replaces the `traffic` field of the request [possible values: auto, historical, time-dependent, off]
      --live <LIVE>
          JSON list of live updates applied before routing
      --live-at <LIVE_AT>
          Receipt time of the live updates (RFC 3339); defaults to now
      --out <OUT>
          Result file (JSON). Omit it to write to the runs directory
  -h, --help
          Print help
```

### `netweevil profile` { #netweevil-profile }

```text
Validate routing profiles and compile them for a dataset

Usage: netweevil profile <COMMAND>

Commands:
  validate  Check a routing profile file for errors
  compile   Compile a routing profile into edge costs for a dataset
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil profile validate` { #netweevil-profile-validate }

```text
Check a routing profile file for errors

Usage: netweevil profile validate <PROFILE>

Arguments:
  <PROFILE>  Routing profile file (YAML or TOML)

Options:
  -h, --help  Print help
```

#### `netweevil profile compile` { #netweevil-profile-compile }

```text
Compile a routing profile into edge costs for a dataset

Usage: netweevil profile compile --dataset <DATASET> --profile <PROFILE>

Options:
      --dataset <DATASET>  Id of an imported dataset
      --profile <PROFILE>  Routing profile file (YAML or TOML)
  -h, --help               Print help
```

### `netweevil analyze` { #netweevil-analyze }

```text
Run routing analyses and save each result as a run

Usage: netweevil analyze <COMMAND>

Commands:
  route                  Route between an origin and a destination, with optional vias
  directions             Route a request and give turn-by-turn instructions, like `POST /v1/directions`
  route-batch            Route each request in a batch file
  od                     Route each origin-destination pair in a pairs file
  matrix                 Calculate travel times and distances from all origins to all destinations
  accessibility          Count the destinations of each category that origins reach within time thresholds
  service-area           Find the network that origins reach within a time or distance limit
  service-area-sequence  Calculate one service area for each departure time in a series
  betweenness            Count how many shortest paths use each edge
  scenario-batch         Route one request under each scenario and compare the results
  transit-route          Find a public transit journey in an imported GTFS feed
  transit-batch          Find a public transit journey for each request in a batch file
  trip                   Put stops in the best visiting order and route the legs
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil analyze route` { #netweevil-analyze-route }

```text
Route between an origin and a destination, with optional vias

Usage: netweevil analyze route [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --request <REQUEST>
          Request file (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze directions` { #netweevil-analyze-directions }

```text
Route a request and give turn-by-turn instructions, like `POST /v1/directions`

Usage: netweevil analyze directions [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --request <REQUEST>
          Route request (.json, .yml or .yaml): a bare route request, or a `/v1/directions` body with `request` and `directions_options`
      --language <LANGUAGE>
          Instruction language as a BCP 47 tag (`nl`, `de-CH`, `zh-HK`). Tags without a phrase table fall back to English with a warning
      --units <UNITS>
          Distance units of the instructions [possible values: metric, imperial]
      --driving-side <DRIVING_SIDE>
          Side of the road traffic keeps to; defaults to the dataset's side at the origin, or right-hand traffic [possible values: left, right]
      --facility-attribute <FACILITY_ATTRIBUTE>
          Feature attribute that classifies stairs, escalators, lifts and moving walkways; defaults to the profile's `facilities.attribute`
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Directions result (.json); defaults to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze route-batch` { #netweevil-analyze-route-batch }

```text
Route each request in a batch file

Usage: netweevil analyze route-batch [OPTIONS] --dataset <DATASET> --profile <PROFILE> --requests <REQUESTS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --requests <REQUESTS>
          Route batch file (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze od` { #netweevil-analyze-od }

```text
Route each origin-destination pair in a pairs file

Usage: netweevil analyze od [OPTIONS] --dataset <DATASET> --profile <PROFILE> --pairs <PAIRS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --pairs <PAIRS>
          Origin-destination pairs (JSON, YAML or CSV)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze matrix` { #netweevil-analyze-matrix }

```text
Calculate travel times and distances from all origins to all destinations

Usage: netweevil analyze matrix [OPTIONS] --dataset <DATASET> --profile <PROFILE> --origins <ORIGINS> --destinations <DESTINATIONS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --origins <ORIGINS>
          Origin point set (JSON, YAML or CSV)
      --destinations <DESTINATIONS>
          Destination point set (JSON, YAML or CSV)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze accessibility` { #netweevil-analyze-accessibility }

```text
Count the destinations of each category that origins reach within time thresholds

Usage: netweevil analyze accessibility [OPTIONS] --dataset <DATASET> --profile <PROFILE> --origins <ORIGINS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --origins <ORIGINS>
          Origin point set (JSON, YAML or CSV)
      --destination <DESTINATIONS>
          Destination category as `category=path` to a point set. Repeat it for each category
      --thresholds-s <THRESHOLDS_S>
          Travel time thresholds in seconds, separated by commas [default: 300 600 900 1200]
      --max-travel-time-s <MAX_TRAVEL_TIME_S>
          Longest travel time in seconds to search from each origin [default: 1200]
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze service-area` { #netweevil-analyze-service-area }

```text
Find the network that origins reach within a time or distance limit

Usage: netweevil analyze service-area [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --request <REQUEST>
          Request file (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze service-area-sequence` { #netweevil-analyze-service-area-sequence }

```text
Calculate one service area for each departure time in a series

Usage: netweevil analyze service-area-sequence [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --request <REQUEST>
          Request file (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze betweenness` { #netweevil-analyze-betweenness }

```text
Count how many shortest paths use each edge

Usage: netweevil analyze betweenness [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --request <REQUEST>
          Request file (JSON or YAML)
      --departure-time <DEPARTURE_TIME>
          Override the request departure time (RFC 3339)
      --scenario <SCENARIO>
          Override the request scenario with a local scenario file (JSON/YAML)
      --holiday-calendar <HOLIDAY_CALENDAR>
          Replace the request holidays with a local holiday calendar (JSON/YAML)
      --overlay <OVERLAYS>
          Append a local temporal overlay file (CSV/Parquet); repeatable
      --out <OUT>
          Result file. The extension selects the format. Omit it to write JSON to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze scenario-batch` { #netweevil-analyze-scenario-batch }

```text
Route one request under each scenario and compare the results

Usage: netweevil analyze scenario-batch [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>  Id of an imported dataset
      --profile <PROFILE>  Routing profile file (YAML or TOML)
      --request <REQUEST>  Scenario batch request (JSON or YAML)
      --out <OUT>          Result file (JSON). Omit it to write to the runs directory
  -h, --help               Print help
```

#### `netweevil analyze transit-route` { #netweevil-analyze-transit-route }

```text
Find a public transit journey in an imported GTFS feed

Usage: netweevil analyze transit-route [OPTIONS] --feed <FEED> --request <REQUEST>

Options:
      --feed <FEED>
          Id of an imported transit feed
      --request <REQUEST>
          Transit route request (JSON)
      --street-dataset <STREET_DATASET>
          Street dataset used when the request selects network access/egress
      --street-profile <STREET_PROFILE>
          Foot profile used when the request selects network access/egress
      --out <OUT>
          Result file (JSON). Omit it to write to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze transit-batch` { #netweevil-analyze-transit-batch }

```text
Find a public transit journey for each request in a batch file

Usage: netweevil analyze transit-batch [OPTIONS] --feed <FEED> --requests <REQUESTS>

Options:
      --feed <FEED>
          Id of an imported transit feed
      --requests <REQUESTS>
          Transit batch file (JSON) with a `requests` list
      --street-dataset <STREET_DATASET>
          Street dataset used by network access/egress requests in the batch
      --street-profile <STREET_PROFILE>
          Foot profile used by network access/egress requests in the batch
      --out <OUT>
          Result file (JSON). Omit it to write to the runs directory
  -h, --help
          Print help
```

#### `netweevil analyze trip` { #netweevil-analyze-trip }

```text
Put stops in the best visiting order and route the legs

Usage: netweevil analyze trip [OPTIONS] --dataset <DATASET> --profile <PROFILE> --request <REQUEST>

Options:
      --dataset <DATASET>  Id of an imported dataset
      --profile <PROFILE>  Routing profile file (YAML or TOML)
      --request <REQUEST>  Trip request (.json, .yml or .yaml)
      --out <OUT>          `.json` writes the full result; `.geojson` writes legs and stops
  -h, --help               Print help
```

### `netweevil experiment` { #netweevil-experiment }

```text
Run reproducible study files

Usage: netweevil experiment <COMMAND>

Commands:
  run   Run every scenario in a study file and write a summary
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil experiment run` { #netweevil-experiment-run }

```text
Run every scenario in a study file and write a summary

Usage: netweevil experiment run <STUDY>

Arguments:
  <STUDY>  Study file (YAML or TOML). Paths in the file are relative to it

Options:
  -h, --help  Print help
```

### `netweevil simulate` { #netweevil-simulate }

```text
Validate and run agent-based traffic simulations

Usage: netweevil simulate <COMMAND>

Commands:
  validate  Validate a simulation scenario file without running it
  run       Run an agent-based traffic simulation scenario
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil simulate validate` { #netweevil-simulate-validate }

```text
Validate a simulation scenario file without running it

Usage: netweevil simulate validate <SCENARIO>

Arguments:
  <SCENARIO>  Scenario file (YAML, TOML or JSON)

Options:
  -h, --help  Print help
```

#### `netweevil simulate run` { #netweevil-simulate-run }

```text
Run an agent-based traffic simulation scenario

Usage: netweevil simulate run [OPTIONS] --dataset <DATASET> <SCENARIO>

Arguments:
  <SCENARIO>  Scenario file (YAML/TOML/JSON)

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILES>
          Profile files; every fleet's profile_id must resolve to one of these (compile them first with `netweevil profile compile`)
      --out-dir <OUT_DIR>
          Output directory (defaults to .netweevil/runs)
      --temporal-geojson
          Also write a temporal GeoJSON of agent frames for the QGIS temporal controller
      --min-traversals <MIN_TRAVERSALS>
          Minimum traversals for an edge to appear in the busy-segments output [default: 1]
  -h, --help
          Print help
```

### `netweevil report` { #netweevil-report }

```text
Render reports from run manifests

Usage: netweevil report <COMMAND>

Commands:
  render  Render a run manifest as a Markdown or HTML report
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil report render` { #netweevil-report-render }

```text
Render a run manifest as a Markdown or HTML report

Usage: netweevil report render [OPTIONS] <MANIFEST>

Arguments:
  <MANIFEST>  Run manifest (JSON) of an analysis or simulation run

Options:
      --out <OUT>  Output path. Use `.md` for Markdown or `.html` for HTML. Any other path becomes a directory with both reports and the run files. Omit it to print Markdown
  -h, --help       Print help
```

### `netweevil cache` { #netweevil-cache }

```text
List and prune compiled profiles and bundle files

Usage: netweevil cache <COMMAND>

Commands:
  list   List datasets, compiled profiles and transit feeds in the workspace
  prune  Remove stale compiled profiles and unreferenced bundle files
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil cache list` { #netweevil-cache-list }

```text
List datasets, compiled profiles and transit feeds in the workspace

Usage: netweevil cache list

Options:
  -h, --help  Print help
```

#### `netweevil cache prune` { #netweevil-cache-prune }

```text
Remove stale compiled profiles and unreferenced bundle files.

A compiled profile is stale when its dataset was removed or re-imported, its bundle is missing or in an old format, or a newer compile of the same profile id exists. Stale profiles recompile on demand. Transit bundles are not touched.

Usage: netweevil cache prune [OPTIONS]

Options:
      --dry-run
          List what would be removed without deleting anything

  -h, --help
          Print help (see a summary with '-h')
```

### `netweevil bench` { #netweevil-bench }

```text
Measure routing speed and quality

Usage: netweevil bench <COMMAND>

Commands:
  corpus   Write a route corpus with pairs sampled over distance bands
  run      Measure a workload in this process with one shared routing engine
  http     Measure a corpus against a running HTTP routing server
  compare  Compare two benchmark JSON reports of the same corpus
  trip     Measure trip optimization quality and latency on real stop matrices
  traffic  Measure bucket customization, live swaps and route latency with traffic
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil bench corpus` { #netweevil-bench-corpus }

```text
Write a route corpus with pairs sampled over distance bands.

The same seed gives the same corpus.

Usage: netweevil bench corpus [OPTIONS] --dataset <DATASET> --profile <PROFILE> --out <OUT>

Options:
      --dataset <DATASET>
          Id of an imported dataset

      --profile <PROFILE>
          Routing profile file (YAML or TOML)

      --count <COUNT>
          Number of routes in the corpus
          
          [default: 1000]

      --seed <SEED>
          Random seed for the sampling
          
          [default: 42]

      --out <OUT>
          Corpus file (CSV)

      --bbox <BBOX>
          Restrict sampling to `min_lon,min_lat,max_lon,max_lat` instead of the full topology bounds

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil bench run` { #netweevil-bench-run }

```text
Measure a workload in this process with one shared routing engine

Usage: netweevil bench run [OPTIONS] --dataset <DATASET> --profile <PROFILE> --corpus <CORPUS>

Options:
      --dataset <DATASET>
          Id of an imported dataset

      --profile <PROFILE>
          Routing profile file (YAML or TOML)

      --corpus <CORPUS>
          Route corpus (CSV) from `bench corpus`

      --engine <ENGINE>
          Routing engine to measure

          Possible values:
          - accelerated: Use the dataset's CCH acceleration bundle when the profile has one
          - exact:       Ignore the acceleration bundle and search the exact graph
          
          [default: accelerated]

      --concurrency <CONCURRENCY>
          Number of requests that run at the same time
          
          [default: 1]

      --warmup <WARMUP>
          Untimed requests executed before the measured phase
          
          [default: 0]

      --iterations <ITERATIONS>
          Number of passes over the corpus in the measured phase
          
          [default: 1]

      --workload <WORKLOAD>
          Kind of request to measure

          Possible values:
          - route:          Point-to-point routes returning summaries only
          - route-geometry: Point-to-point routes returning full geometry
          - matrix:         One NxN travel-time matrix over the first N corpus origins
          - service-area:   One service area per corpus row, seeded from its origin
          
          [default: route]

      --limit <LIMIT>
          Only use the first N corpus rows

      --matrix-size <MATRIX_SIZE>
          Side length of the `matrix` workload
          
          [default: 100]

      --threshold-s <THRESHOLD_S>
          Travel-time cutoff of the `service-area` workload, in seconds
          
          [default: 900]

      --service-area-output <SERVICE_AREA_OUTPUT>
          Output of the `service-area` workload: reachable network segments only, or traced isochrone polygons with geometry

          Possible values:
          - network: Reachable network summary without geometry
          - polygon: Isochrone polygons with geometry
          
          [default: network]

      --json <JSON>
          Write the machine-readable report here

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil bench http` { #netweevil-bench-http }

```text
Measure a corpus against a running HTTP routing server

Usage: netweevil bench http [OPTIONS] --corpus <CORPUS>

Options:
      --url <URL>
          Base URL of the routing server
          
          [default: http://127.0.0.1:8080]

      --corpus <CORPUS>
          Route corpus (CSV) from `bench corpus`

      --backend <BACKEND>
          API that the server provides

          Possible values:
          - netweevil: The NetWeevil HTTP API (`POST /v1/route`)
          - osrm:      An OSRM `route` service (`GET /route/v1/{profile}/...`)
          - valhalla:  A Valhalla route service (`POST /route`)
          
          [default: netweevil]

      --concurrency <CONCURRENCY>
          Number of requests that run at the same time
          
          [default: 1]

      --warmup <WARMUP>
          Untimed requests executed before the measured phase
          
          [default: 0]

      --iterations <ITERATIONS>
          Number of passes over the corpus in the measured phase
          
          [default: 1]

      --limit <LIMIT>
          Only use the first N corpus rows

      --profile-id <PROFILE_ID>
          `profile_id` sent to the NetWeevil API; omitted means the server default

      --osrm-profile <OSRM_PROFILE>
          OSRM profile path segment
          
          [default: driving]

      --valhalla-costing <VALHALLA_COSTING>
          Valhalla costing model, such as auto, bicycle, or pedestrian
          
          [default: auto]

      --request-interval-ms <REQUEST_INTERVAL_MS>
          Minimum interval between HTTP request starts; requires --concurrency 1
          
          [default: 0]

      --timeout-s <TIMEOUT_S>
          Per-request timeout in seconds
          
          [default: 300]

      --json <JSON>
          Write the machine-readable report here

  -h, --help
          Print help (see a summary with '-h')
```

#### `netweevil bench compare` { #netweevil-bench-compare }

```text
Compare two benchmark JSON reports of the same corpus

Usage: netweevil bench compare --baseline <BASELINE> --candidate <CANDIDATE>

Options:
      --baseline <BASELINE>    Reference report
      --candidate <CANDIDATE>  Report being judged against the baseline
  -h, --help                   Print help
```

#### `netweevil bench trip` { #netweevil-bench-trip }

```text
Measure trip optimization quality and latency on real stop matrices

Usage: netweevil bench trip [OPTIONS] --dataset <DATASET> --profile <PROFILE> --corpus <CORPUS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML)
      --corpus <CORPUS>
          Stops are the first N corpus origins
      --sizes <SIZES>
          Stop counts to measure, separated by commas [default: 10 50 200 1000]
      --reference-runs <REFERENCE_RUNS>
          Seeded runs, each with `--reference-factor` times the default iterations, whose best order is the reference for large sizes [default: 8]
      --reference-factor <REFERENCE_FACTOR>
          Iteration multiplier of each reference run [default: 5]
      --bound-max-size <BOUND_MAX_SIZE>
          Skip the O(n³) assignment bound above this size [default: 1000]
      --json <JSON>
          Write the machine-readable report here
  -h, --help
          Print help
```

#### `netweevil bench traffic` { #netweevil-bench-traffic }

```text
Measure bucket customization, live swaps and route latency with traffic

Usage: netweevil bench traffic [OPTIONS] --dataset <DATASET> --profile <PROFILE> --traffic <TRAFFIC> --corpus <CORPUS>

Options:
      --dataset <DATASET>
          Id of an imported dataset
      --profile <PROFILE>
          Routing profile file (YAML or TOML) of a motorized mode
      --traffic <TRAFFIC>
          Id of an imported historical traffic set
      --corpus <CORPUS>
          Route corpus (CSV) from `bench corpus`
      --departures <DEPARTURES>
          Departures whose buckets are customized and routed [default: 2026-10-05T08:15:00+02:00 2026-10-05T12:30:00+02:00 2026-10-05T17:30:00+02:00 2026-10-04T03:00:00+02:00]
      --live-batches <LIVE_BATCHES>
          Live batch sizes to publish (alternating closures and slowdowns) [default: 1 100 1000]
      --verify-exact
          Compare bucket routes with the exact engine on the same metric
      --time-dependent-routes <TIME_DEPENDENT_ROUTES>
          Corpus routes to time with the exact time-dependent engine [default: 0]
      --passes <PASSES>
          Number of passes over the corpus for each measurement [default: 5]
      --json <JSON>
          Write the machine-readable report here
  -h, --help
          Print help
```

### `netweevil api` { #netweevil-api }

```text
Run the HTTP API server

Usage: netweevil api <COMMAND>

Commands:
  serve  Start the HTTP API and the web console
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `netweevil api serve` { #netweevil-api-serve }

```text
Start the HTTP API and the web console.

Without a dataset, the API starts in setup mode.

Usage: netweevil api serve [OPTIONS]

Options:
      --dataset <DATASET>
          Dataset to load. Without it the selection saved in `.netweevil/workspace.json` is used, or the API starts in setup mode

      --default-profile <DEFAULT_PROFILE>
          Routing profile file that requests use when they name no profile

      --bind <BIND>
          Address and port to listen on
          
          [default: 127.0.0.1:8080]

      --profile <PROFILES>
          Additional routing profile file to load; repeatable

      --transit-feed <TRANSIT_FEEDS>
          Id of an imported transit feed to load; repeatable

      --traffic <TRAFFIC>
          Historical traffic set (`netweevil traffic import`) for departure times; live updates work without one

      --console-dir <CONSOLE_DIR>
          Built web console (`frontend/dist`) to serve at `/`; auto-detected

  -h, --help
          Print help (see a summary with '-h')
```

### `netweevil bootstrap` { #netweevil-bootstrap }

```text
Start the API and web console in setup mode for a new workspace

Usage: netweevil bootstrap [OPTIONS]

Options:
      --bind <BIND>                Address and port to listen on [default: 127.0.0.1:8080]
      --console-dir <CONSOLE_DIR>  Built web console (`frontend/dist`) to serve at `/`; the copy embedded in this binary is used when present
      --no-open                    Do not open the console in the default browser
  -h, --help                       Print help
```

