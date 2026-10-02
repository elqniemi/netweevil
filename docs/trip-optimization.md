# Trip optimization

`POST /v1/trip`, `netweevil analyze trip` and waypoint `optimize_order` share
one solver (`crates/query/src/trip/solver.rs`). This note records how it works
and how good it is.

## Pipeline

1. Each stop is snapped twice, as a departure and as an arrival, with the
   candidates `/v1/route` uses. A role that cannot snap (arriving where a
   one-way street starts) is left empty, so the stop can still open or close
   an open trip.
2. One directed matrix of generalized cost and travel time comes from the
   parallel CCH many-to-many engine. Rows are routed in chunks of at most
   200,000 cells and reduced to two numbers per cell immediately, so a
   1,000-stop trip peaks at 1.9 GB including the Groningen engine, against
   4.1 GB for a full `/v1/matrix` of the same size.
3. Every mode becomes a tour through a depot (node 0) whose row and column
   encode the ends: a roundtrip keeps the start stop; a free start or end
   costs nothing to leave or reach; fixed endpoints merge start and end into
   one depot that leaves like the start and is reached like the end.
4. Only the chosen legs are routed again, with the requested returns.

## Solver

- **Exact.** Up to 16 free stops (`exact_max_stops`, at most 18) Held–Karp
  minimizes directed cost. With deadlines, each (visited set, last stop)
  keeps the Pareto set of (cost, departure time) labels. Waiting is allowed,
  so an earlier departure at no greater cost is never worse and the labels
  are exact. If no label survives, the instance is proven infeasible.
- **Heuristic.** Above that, the best of four constructions (input order,
  nearest neighbor, greedy arc matching, and earliest deadline when windows
  apply) is improved by iterated local search:
  - 2-opt, priced in O(1) with forward and backward prefix sums, because a
    reversed segment costs differently on a directed network;
  - Or-opt of one to three stops in either orientation;
  - or-3opt exchange of two adjacent segments, which never reverses anything
    and is the most useful move on strongly asymmetric costs.

  Candidates come from 10 nearest out- and in-neighbors per stop, processed
  from a work queue (don't-look bits). Each round applies a localized double
  bridge (two adjacent segments of at most 30 stops swapped) and keeps the
  result unless it is worse. The run stops after `max_iterations` kicks
  (default `clamp(20 n, 2000, 40000)`) or after `time_budget_ms`. Iteration
  limits make results deterministic for a seed; a time budget may not.
- **Objective.** Lexicographic: legs without a route, then total lateness,
  then generalized cost. With deadlines a candidate's lateness is
  re-simulated from the first changed position. The walk stops once the
  unchanged tail is reached no later than before, because departures are
  monotone in the previous departure.

## Measurements

`netweevil bench trip` on Groningen (`car_research_v1`, 1.04 M edge states)
with roundtrips over random province-wide stops, release build, 10 cores on a
shared machine (load average about 30):

| stops | matrix | solve | end to end | method | vs reference | vs assignment bound |
| ---: | ---: | ---: | ---: | --- | ---: | ---: |
| 10 | 7 ms | 0.1 ms | 10 ms | Held–Karp | optimal | +12.7% |
| 50 | 13 ms | 36 ms | 54 ms | local search | +0.00% | +15.0% |
| 200 | 47 ms | 96 ms | 158 ms | local search | +0.83% | +12.8% |
| 1000 | 1.4 s | 1.1–1.8 s | 3.3 s | local search | +0.98% | +19.2% |

The reference is the best of the default run and four seeded runs with five
times the iterations each. The assignment bound is far from tight on road
networks, so it only caps the gap. On 10 stops, local search alone finds the
Held–Karp optimum. In unit tests on random asymmetric instances of 8–12 stops,
local search must match Held–Karp on at least 95% of instances and stay
within 2% on every one. Local search starts
from the input order among its constructions, so it never returns a worse
order.

## Limits

- Costs are static. Departure times, scenarios and traffic overlays are not
  applied to the matrix or to the schedule.
- One vehicle, hard windows, no capacities. Multi-vehicle capacitated routing
  is not implemented.
- Above roughly 1,000 stops the matrix dominates latency; 2,000 stops is the
  request limit.
