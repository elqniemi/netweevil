# Trip optimization

Trip optimization puts stops in the best visiting order and routes the legs.
It is similar to OSRM `/trip` or Valhalla `optimized_route`. It supports up
to 2,000 stops, service times and time windows.

`POST /v1/trip`, `netweevil analyze trip` and waypoint `optimize_order` use
the same solver.

## Prerequisites

- An imported dataset and a compiled profile.
- For HTTP requests: a running API that loads the profile.

## Optimize a trip

=== "CLI"

    ```bash
    cargo run -p netweevil-cli -- analyze trip \
      --dataset groningen_2026_05 \
      --profile examples/profiles/car_research_v1.yml \
      --request examples/requests/trip_groningen.json \
      --out .netweevil/runs/trip.geojson
    ```

    `.json` writes the full result. `.geojson` writes the legs and stops.

=== "HTTP"

    The envelope is `{profile_id?, request}`. Add `?format=geojson` for
    GeoJSON.

    ```bash
    curl -X POST 'http://127.0.0.1:8080/v1/trip?format=geojson' \
      -H 'Content-Type: application/json' \
      --data '{"request": '"$(cat examples/requests/trip_groningen.json)"'}'
    ```

## Request fields

| Field | Default | Meaning |
| --- | --- | --- |
| `trip_id` | required | Identifier of the result |
| `stops[]` | required | `{point, service_s?, time_window?}` |
| `stops[].service_s` | 0 | Service time at the stop in seconds |
| `stops[].time_window` | none | `{earliest_s, latest_s?}` |
| `mode` | `roundtrip` | See [Trip modes](#trip-modes) |
| `departure_s` | 0 | Earliest start of the trip |
| `allow_late` | `false` | Returns the least-late order instead of an error |
| `snap`, `returns` | defaults | As for a route |
| `optimizer` | defaults | See [Optimizer options](#optimizer-options) |
| `vehicle` | profile vehicle | As for a route |

Times (`service_s`, `earliest_s`, `latest_s`, `departure_s`) are seconds on
one clock, for example seconds since midnight. A vehicle that arrives early
waits. Service must start by `latest_s`.

### Trip modes

| Mode | Start | End |
| --- | --- | --- |
| `roundtrip` | First stop | Returns to the first stop |
| `fixed_start` | First stop | Any stop |
| `fixed_end` | Any stop | Last stop |
| `fixed_endpoints` | First stop | Last stop |
| `open` | Any stop | Any stop |

### Optimizer options

| Field | Default | Meaning |
| --- | --- | --- |
| `seed` | 0 | Seed for the local-search kicks |
| `max_iterations` | `clamp(20 n, 2000, 40000)` | Number of local-search kicks |
| `time_budget_ms` | none | Wall-clock limit of the search |
| `exact_max_stops` | 16 | Largest number of free stops for the exact solver. The maximum is 18 |

!!! note
    Equal seeds give equal orders. A time budget can stop the search first,
    and then the order can change between runs.

### Late stops

If no order meets every time window, the request fails. The error names the
late stops. It also names the stops that are unreachable in time even
directly from the start. Set `allow_late: true` to get the least-late order.
The result then has `late_stops` and a `lateness_s` value for each visit.

## Result

| Field | Meaning |
| --- | --- |
| `stop_order` | Request indexes in visit order. A roundtrip ends with its start |
| routes | One route per leg |
| `visits` | Arrival, wait, service and departure times of each stop |
| totals | Totals of distance, time and cost |
| `optimization` | Method, proof of optimality, input-order cost, iterations, and matrix, solve and leg timings |

Each stop uses the same snapping as `/v1/route`.

## How the solver works

1. The engine snaps each stop twice: as a departure and as an arrival. It
   uses the candidates of `/v1/route`. A role that cannot snap stays empty.
   An example is arrival where a one-way street starts. Such a stop can still
   open or close an open trip.
2. The parallel CCH many-to-many engine calculates one directed matrix of
   generalized cost and travel time. It routes rows in chunks of at most
   200,000 cells. It reduces each cell to two numbers at once. Thus a
   1,000-stop trip needs about half the memory of a full `/v1/matrix` of the
   same size.
3. The solver turns each mode into a tour through a depot (node 0). The row
   and column of the depot encode the ends:
    - a roundtrip keeps the start stop;
    - a free start or end costs nothing to leave or reach;
    - fixed endpoints merge start and end into one depot. The depot leaves
      like the start and is reached like the end.
4. The engine routes only the chosen legs again, with the requested returns.

### Exact solver

Up to `exact_max_stops` free stops, Held–Karp minimizes the directed cost.
With deadlines, each (visited set, last stop) keeps the Pareto set of (cost,
departure time) labels. Waiting is allowed. Thus an earlier departure at no
greater cost is never worse, and the labels are exact. If no label survives,
the instance is proven infeasible.

### Heuristic solver

Above that size, the solver takes the best of four constructions:

- input order;
- nearest neighbor;
- greedy arc matching;
- earliest deadline, when windows apply.

Iterated local search then improves the order with these moves:

- 2-opt, priced in O(1) with forward and backward prefix sums. A reversed
  segment has a different cost on a directed network.
- Or-opt of one to three stops in either orientation.
- Or-3opt exchange of two adjacent segments. This move never reverses a
  segment. It is the most useful move on strongly asymmetric costs.

Candidates come from the 10 nearest out-neighbors and in-neighbors of each
stop. A work queue (don't-look bits) processes them. Each round applies a
localized double bridge: it swaps two adjacent segments of at most 30 stops.
The solver keeps the result unless it is worse. The run stops after
`max_iterations` kicks or after `time_budget_ms`.

The local search starts from the input order among its constructions. Thus it
never returns an order that is worse than the input order.

### Objective

The objective is lexicographic:

1. legs without a route;
2. total lateness;
3. generalized cost.

With deadlines, the solver simulates the lateness of a candidate again from
the first changed position. The walk stops when it reaches the unchanged tail
no later than before. Departures are monotone in the previous departure.

## Quality

`netweevil bench trip` measures the solver on real stop matrices. See
[Benchmarking](benchmarking.md#benchmark-trip-optimization).

As a guide, on a province-size road network:

- At 10 stops, local search alone finds the Held–Karp optimum.
- At 50 to 1,000 stops, the default run is within about 1% of the best of
  several longer seeded runs.
- The assignment lower bound is not tight on road networks. It only caps the
  gap.
- Above about 1,000 stops, the matrix calculation takes most of the time.

Unit tests on random asymmetric instances of 8 to 12 stops check that local
search matches Held–Karp on at least 95% of instances. It must stay within 2%
on every instance.

## Limits

- Costs are static. The solver does not apply departure times, scenarios or
  traffic to the matrix or to the schedule.
- The solver plans one vehicle with hard windows and no capacities.
  Multi-vehicle capacitated routing is not available.
- A request can have at most 2,000 stops.

## Related pages

- [Routing](routing.md#routes-with-intermediate-locations) for waypoint
  `optimize_order`.
- [Batch analysis](batch-analysis.md) for matrices.
- [HTTP API reference](../api/reference.md) for the full schema.
