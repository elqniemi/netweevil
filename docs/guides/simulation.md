# Agent-based traffic simulation

The simulation moves fleets of agents over the directed edge graph of a
dataset. Each fleet uses one compiled profile. The engine is a mesoscopic
queue model:

- Each edge has a storage capacity (the PCU that fit on it) and an outflow
  capacity (PCU per second).
- Speeds drop with density (Greenshields or BPR).
- With `no_collision`, full downstream edges block transitions (spillback).
- Edges with traffic signals gate their outflow on a fixed cycle.
- Agents in a jam can reroute with live congested costs.

A run is deterministic for a given scenario, dataset, profiles and seed. The
CLI, the API and the QGIS plugin use the same scenario schema.

```mermaid
flowchart TD
    A[Scenario file] --> B[Sample origins, destinations and departures<br/>from the seed]
    B --> C[Route the next departure window]
    C --> D[Tick: move agents along edges]
    D --> E[Update density, speeds, queues and signals]
    E --> F{Agent in a jam?}
    F -->|yes, within reroute budget| G[Reroute with congested costs]
    F -->|no| H[Record frames and edge statistics]
    G --> H
    H --> I{All arrived or duration reached?}
    I -->|no| C
    I -->|yes| J[Write result, busy segments, congestion<br/>and optional temporal GeoJSON]
```

The engine plans all agents at the start, but it routes them in windows of
departures ahead of the clock. Thus the simulation starts at once.

## Prerequisites

- An imported dataset.
- A compiled profile for each fleet. Each fleet's `profile_id` must match a
  profile that you pass to the CLI or that the API loads.

## Run a simulation

=== "CLI"

    1. Validate the scenario:

        ```bash
        cargo run -p netweevil-cli -- simulate validate \
          examples/requests/simulation_scenario.yml
        ```

    2. Compile each profile that the fleets use.
    3. Run the scenario:

        ```bash
        cargo run -p netweevil-cli -- simulate run \
          examples/requests/simulation_scenario.yml \
          --dataset groningen_2026_05 \
          --profile examples/profiles/car_research_v1.yml \
          --profile examples/profiles/cycling_north_nl_2026_05.yml \
          --temporal-geojson \
          --out-dir .netweevil/runs
        ```

=== "HTTP"

    1. Start the API with the profiles of the fleets.
    2. Send `{"scenario": <scenario document as JSON>}` to
       `POST /v1/simulation`. The response has the `simulation_id` and the
       status.
    3. Watch the run with the frame, edge and status endpoints, or in the
       QGIS plugin's Simulation tab.

| Flag | Default | Meaning |
| --- | --- | --- |
| `<SCENARIO>` | required | Scenario file (YAML, TOML or JSON) |
| `--dataset` | required | Dataset id |
| `--profile` | none | Profile files. Repeat for each fleet profile |
| `--out-dir` | `.netweevil/runs` | Output directory |
| `--temporal-geojson` | off | Also writes a temporal GeoJSON of agent frames for the QGIS temporal controller |
| `--min-traversals` | 1 | Smallest number of traversals for an edge in the busy-segments output |

### Output files

For a scenario with id `<id>`, the CLI writes these files to the output
directory:

| File | Contents |
| --- | --- |
| `simulation-<id>-result.json` | Compact JSON result |
| `simulation-<id>-busy-segments.geojson` | Busy segments |
| `simulation-<id>-congestion.geojson` | Congestion |
| `simulation-<id>-temporal.geojson` | Agent frames, with `--temporal-geojson` |

The CLI also writes a run manifest. See
[Reports and experiments](reports-and-experiments.md).

## Scenario document

`examples/requests/simulation_scenario.yml` dispatches 600 cars and 300
bicycles over 30 simulated minutes. It has a slow zone in the city center and
standing background traffic in an area of the ring road.

### `scenario`

| Field | Default | Meaning |
| --- | --- | --- |
| `id` | required | Scenario id |
| `label`, `description` | empty | Text |
| `seed` | 1 | Random seed. The same seed gives an identical run |

### `time`

| Field | Default | Meaning |
| --- | --- | --- |
| `duration_s` | 3600 | Simulated horizon in seconds |
| `tick_s` | 1.0 | Engine tick. A smaller tick gives finer movement and a slower run |
| `stop_when_all_arrived` | `true` | Ends the run when all agents arrive |

### `fleets[]`

| Field | Meaning |
| --- | --- |
| `fleet_id` | Fleet id |
| `profile_id` | Compiled profile. The travel mode comes from the profile |
| `agent_count` | Number of agents |
| `demand` | Origins, destinations or explicit OD pairs |
| `departures` | Departure time distribution |
| `behavior` | Agent behavior |
| `speed` | Speed and acceleration |
| `vehicle_types` | Vehicle classes of the fleet. See [Vehicle types](#vehicle-types) |
| `ride_hailing` | Makes the fleet a ride-hailing service. See [Ride-hailing fleets](#ride-hailing-fleets) |

`demand.origins` and `demand.destinations` take a `kind`:

| `kind` | Fields | Meaning |
| --- | --- | --- |
| `random_bounds` (default) | none | Uniform inside the dataset extent |
| `random_bbox` | `bbox: [min_lon, min_lat, max_lon, max_lat]` | Uniform inside a box |
| `random_polygon` | `polygon` | Uniform inside a WGS84 outer ring |
| `points` | `points: [{lon, lat, weight?, label?}]` | Weighted choice from fixed points |
| `zones` | `zone_ids` | Inside named `spawn` or `attract` zones, weighted |
| `cells` | `cells: [{cell, weight?}]` | Weighted choice of an [H3 cell](h3-cells.md), then uniform inside the cell |
| `cell_layer` | `layer_id`, `column` | The cells and weights of one column of a stored [cell layer](h3-cells.md#cell-layers) |

With `cells`, the weights are a distribution, for example the population of
each cell for origins and the number of jobs for destinations. The cells can
have different resolutions. `netweevil h3 aggregate` with a `.json` output
writes a list that you can use as `cells`.

`demand.od_pairs` is a list of `{origin: [lon, lat], destination: [lon,
lat]}`. Explicit pairs replace origins and destinations. The engine cycles
them when there are fewer pairs than agents.

`departures` take a `kind`:

| `kind` | Fields | Meaning |
| --- | --- | --- |
| `uniform` (default `0` to `600`) | `start_s`, `end_s` | Uniform in `[start_s, end_s)` |
| `peak` | `mean_s`, `std_s` | Normal distribution around a peak, clamped at zero |
| `instant` | `at_s` | All agents at one instant |
| `poisson` | `start_s`, `rate_per_s` | Poisson arrivals |

`behavior` values are fleet means. Each agent varies them deterministically
by `heterogeneity`.

| Field | Default | Meaning |
| --- | --- | --- |
| `overtake_eagerness` | 0.4 | 0 to 1. How much of its desired speed an agent keeps in moderate congestion |
| `reroute_eagerness` | 0.3 | 0 to 1. Probability that an agent reroutes in a jam |
| `jam_speed_threshold` | 0.4 | Speed fraction of free flow that counts as a jam |
| `reroute_cooldown_s` | 120 | Minimum time between reroutes of one agent |
| `obey_traffic_signals` | `true` | Stops at red signals |
| `alternative_route_share` | 0.25 | 0 to 1. Share of agents that start on a ranked alternative route |
| `max_alternatives` | 3 | Alternatives requested at dispatch |
| `no_collision` | `true` | Enforces storage capacity, queues and spillback. Set `false` for ghost flows |
| `heterogeneity` | 0.15 | 0 to 1. Relative variation per agent |

!!! note
    An alternative search costs about ten times more than a plain route.
    With fully random demand, each agent is a unique pair. Thus a high
    `alternative_route_share` makes dispatch slow. Demand with shared
    endpoints (points, zones or OD pairs) shares this cost per unique pair.

`speed`:

| Field | Default | Meaning |
| --- | --- | --- |
| `speed_multiplier_mean` | 1.0 | Mean multiplier on profile speeds |
| `speed_multiplier_std` | 0.08 | Standard deviation of the multiplier |
| `max_speed_kph` | none | Hard speed limit |
| `acceleration_mps2` | 2.0 | For the trapezoidal position model and spawn ramp-up |
| `deceleration_mps2` | 2.5 | For the trapezoidal position model |
| `pcu` | by mode | Passenger-car units of one vehicle: car and taxi 1.0, HGV and bus 2.0, motorcycle 0.5, moped and bicycle 0.3, foot 0.1 |

### Vehicle types

`vehicle_types` divides a fleet into vehicle classes, for example compact
cars, SUVs and delivery vans. Each agent gets one class by `share`. The
routing profile of the fleet applies to all classes.

```yaml
vehicle_types:
  - { type_id: compact, share: 5.0, pcu: 0.9 }
  - { type_id: suv, share: 3.0, pcu: 1.2 }
  - { type_id: delivery_van, share: 2.0, pcu: 1.6, max_speed_kph: 80.0, speed_factor: 0.9 }
```

| Field | Default | Meaning |
| --- | --- | --- |
| `type_id` | required | Id of the class |
| `share` | 1.0 | Relative share of the vehicles of the fleet |
| `pcu` | fleet value | Passenger-car units of one vehicle. A larger vehicle uses more storage and flow capacity |
| `max_speed_kph` | fleet value | Speed limit of the vehicle |
| `speed_factor` | 1.0 | Multiplier on the desired speed |
| `request_share` | 0 | Ride-hailing only. Relative share of the requests that need this class |

The result has `vehicle_types` for each fleet: the number of dispatched and
arrived agents and the mean travel time and distance of each class. Each
trajectory has its `vehicle_type`. A fleet without `vehicle_types` runs as
before.

For classes with different access rules or speeds on the network, for
example cars and trucks, use one fleet for each routing profile.

### Ride-hailing fleets

`ride_hailing` makes a fleet a ride-hailing service. `agent_count` is then
the number of ride requests. `demand.origins` gives the pickups,
`demand.destinations` gives the dropoffs and `departures` gives the request
times.

```yaml
ride_hailing:
  vehicle_count: 40
  vehicles: { kind: random_bbox, bbox: [6.53, 53.20, 6.60, 53.24] }
  max_wait_s: 600.0
  surge:
    interval_s: 300.0
    price_elasticity: 0.5
    config: { resolution: 8, max_multiplier: 2.5 }
```

| Field | Default | Meaning |
| --- | --- | --- |
| `vehicle_count` | required | Number of vehicles |
| `vehicles` | `random_bounds` | Where the vehicles start. Takes the same kinds as `demand.origins` |
| `max_wait_s` | 600 | A request without a vehicle within this estimated wait is not served |
| `planning_speed_kph` | 25 | Speed for the estimates of pickup times and trip times in the plan |
| `surge.interval_s` | 300 | Length of one surge period |
| `surge.price_elasticity` | 0 | A request at the multiplier `m` is dropped with the probability `1 - m^-elasticity` |
| `surge.config` | defaults | The [surge settings](h3-cells.md#surge-balance-demand-and-supply) |

The engine makes the plan before the run. It serves the requests in the
order of their times. Each request gets the vehicle with the earliest
estimated arrival at the pickup. The vehicle drives to the pickup and then
to the dropoff as one agent. After the estimated end of the trip, the
vehicle is free at the dropoff.

In each surge period, the demand is the requests of the period, and the
supply is the vehicles that are free at the start of the period. With
`vehicle_types`, each vehicle has a class. With `request_share`, a request
needs a vehicle of its class.

The fleet result has `ride_hailing`:

| Field | Meaning |
| --- | --- |
| `requests`, `vehicles` | Sizes of the demand and the vehicle pool |
| `served` | Requests with a vehicle in the plan. These are the agents of the fleet |
| `unserved` | Requests without a vehicle within `max_wait_s` |
| `priced_out` | Requests dropped by the price elasticity |
| `picked_up`, `completed` | Riders picked up and trips finished in the simulated time |
| `mean_planned_wait_s`, `mean_wait_s` | Mean wait from request to pickup: estimated in the plan, and simulated with congestion |
| `mean_surge_multiplier` | Mean multiplier of the served requests |
| `surge_periods[]` | `{start_s, end_s, cells[]}` with the demand, supply, ratio and multiplier of each cell |

Each trajectory of the fleet has `request_s` and `pickup_s`.

!!! note
    The plan uses estimated times. Congestion in the run delays pickups and
    trips, but it does not change which vehicle serves which request. A
    rerouted vehicle keeps its pickup.

`examples/requests/simulation_ride_hailing.yml` is a scenario with a
ride-hailing fleet, surge and background traffic of three vehicle types.

### `zones[]`

A zone is a WGS84 polygon or a set of H3 cells, with an effect. You can draw
polygon zones in QGIS or write zones in the scenario file.

| Field | Meaning |
| --- | --- |
| `zone_id` | Zone id |
| `label` | Text |
| `polygon` | Outer ring as `[lon, lat]` pairs, open or closed |
| `cells` | H3 cell ids, as an alternative to `polygon`. The resolutions can be different |
| `effect` | See the effect kinds |

| Effect `kind` | Fields | Meaning |
| --- | --- | --- |
| `no_access` | `modes` | Closes edges inside for the listed modes. An empty list closes them for all modes |
| `speed_factor` | `factor` | Multiplies edge speeds inside |
| `capacity_factor` | `factor` | Multiplies storage and flow capacity inside, for example lane closures |
| `high_traffic` | `background_load` | Standing background traffic as a fraction (0 to 1) of storage capacity |
| `spawn` | `weight` | Origin sampling zone for `zones` demand |
| `attract` | `weight` | Destination sampling zone for `zones` demand |

### `traffic`

| Field | Default | Meaning |
| --- | --- | --- |
| `model` | `greenshields` | `greenshields`: `v = v_free * max(min_speed_factor, 1 - density_ratio)`. `bpr`: `v = v_free / (1 + alpha * density_ratio^beta)` |
| `bpr_alpha`, `bpr_beta` | 1.0, 4.0 | BPR parameters |
| `min_speed_factor` | 0.05 | Lowest congested speed as a fraction of free flow |
| `background_load` | 0.0 | Standing load on every edge, 0 to 1 of storage |
| `signal_cycle_s` | 60 | Signal cycle length |
| `signal_green_share` | 0.55 | Green share of the cycle |
| `reroute_budget_per_s` | 80 | Largest number of reroutes per simulated second |

### `output`

| Field | Default | Meaning |
| --- | --- | --- |
| `frame_interval_s` | 5.0 | Interval between agent-position frames |
| `frame_max_agents` | 20,000 | Agents per frame. A deterministic stride sample applies above this count |
| `edge_stats_interval_s` | 60 | Width of the congestion bins |
| `store_trajectories` | `true` | Keeps full agent trajectories in the result |
| `position_model` | `linear` | `linear` or `trapezoidal` (smooth acceleration for playback) |

## Simulation API

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/simulation` | Lists simulations |
| `POST /v1/simulation` | Starts a simulation from `{scenario}` |
| `GET /v1/simulation/{simulation_id}` | Status |
| `DELETE /v1/simulation/{simulation_id}` | Removes a simulation |
| `POST /v1/simulation/{simulation_id}/control` | Sends a control command |
| `GET /v1/simulation/{simulation_id}/frames` | Agent frames |
| `GET /v1/simulation/{simulation_id}/edges` | Edge statistics |
| `GET /v1/simulation/{simulation_id}/temporal` | Temporal GeoJSON |
| `GET /v1/simulation/{simulation_id}/agents/{agent_id}` | One agent |

### Control commands

Send one command as `{"command": "<name>", ...}`. Commands work while a
simulation runs or is paused.

| `command` | Fields | Effect |
| --- | --- | --- |
| `pause` | none | Pauses the run |
| `resume` | none | Resumes the run |
| `cancel` | none | Cancels the run |
| `set_global_speed_factor` | `factor` | Scales all edge speeds, for example for weather. `1.0` restores normal speeds |
| `add_zone` | `zone` | Adds a zone. It takes effect on the next tick |
| `remove_zone` | `zone_id` | Removes a zone |
| `spawn_fleet` | `fleet` | Dispatches an extra fleet. Departure times are relative to the current simulation time |

### Query parameters

| Endpoint | Parameters |
| --- | --- |
| `frames` | `start_s`, `end_s`, `max_agents`, `format` (`json` columnar by default, or `geojson` for the last frame only) |
| `edges` | `start_s`, `end_s`, `min_occupancy` (mean PCU), `mode` (`bins` by default, or `summary` for busy segments of the full run), `min_traversals` |
| `temporal` | `start_s`, `end_s`, `max_agents`, `base_datetime` (reference instant of frame timestamps, default `2026-01-01T00:00:00`) |

The run state is `pending`, `dispatching`, `running`, `paused`, `completed`,
`cancelled` or `failed`.

## Related pages

- [Traffic](traffic.md) for historical and live speeds in routing.
- [Reports and experiments](reports-and-experiments.md).
- [HTTP API reference](../api/reference.md) for the full schema.
