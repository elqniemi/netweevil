# GPS trace matching

Trace matching attaches a sequence of GPS observations to the roads of a
prepared street profile. Use it to rebuild the path of a vehicle or a person
from noisy positions.

The matcher uses a hidden Markov model with a Viterbi search:

- Gaussian penalties score the positional error of each candidate.
- Transition penalties score the difference between the observed
  displacement and the shortest legal network distance.
- Viterbi states keep the turn-restriction history across observations,
  including multi-edge restrictions.

The transition search minimizes distance in metres over the edges that the
profile allows. It does not minimize the profile's time or generalized cost.

## Prerequisites

- A running API with the profile loaded. Trace matching is available on the
  HTTP API only.
- A trace of 2 to 512 observations.

## Match a trace

1. Write the request. Put each position in `observations[].point`.
2. Send it to `POST /v1/match`:

    ```bash
    curl -X POST http://127.0.0.1:8080/v1/match \
      -H 'content-type: application/json' \
      --data @examples/api/north_nl_match.json
    ```

3. Add `?format=geojson` to get GeoJSON.

The envelope is `{profile_id?, request}`.

## Request fields

| Field | Default | Meaning |
| --- | ---: | --- |
| `trace_id` | required | Identifier of the trace |
| `observations[]` | required | `{point, timestamp_s?, accuracy_m?}`. `point` is `{id, lon, lat, z?}` |
| `snap.max_distance_m` | 50 | Candidate search radius in metres |
| `max_candidates` | 8 | Largest number of directed candidates per observation, from 1 to 8 |
| `gps_accuracy_m` | 10 | Standard deviation of the positional error in metres |
| `transition_beta_m` | 50 | Scale of the network-distance discrepancy penalty |
| `max_transition_distance_m` | 20,000 | Largest legal distance between adjacent matched observations |
| `max_time_gap_s` | 60 | Splits a timestamped trace at larger gaps |
| `max_speed_mps` | 70 | Additional distance limit from the elapsed seconds |
| `max_transition_states` | 100,000 | Hard search limit per origin candidate and restriction state |
| `max_search_states` | 2,000,000 | Hard total search-state budget for the request |
| `avoid`, `vehicle` | none | As for a route |

`accuracy_m` on an observation replaces `gps_accuracy_m` for that
observation. `snap` accepts the same elevation, attribute, bearing and
road-side constraints as other analyses. See
[Snap constraints](snap-constraints.md).

### Timestamps

- Supply `timestamp_s` for all observations or for none.
- Timestamps must be finite and strictly increasing.
- They can use any common time origin in seconds.

Timestamps limit continuity and speed. The matcher does not use temporal road
costs.

!!! warning
    When a search exceeds `max_transition_states` or `max_search_states`, the
    request fails. The matcher does not return a partly searched optimum.

## Result

The response is `{service, result}`.

- `result.tracepoints` aligns with the input. Each entry is a matched network
  position, or `null` for an unmatched point.
- `matchings` contains connected sequences. Each sequence has observation
  indexes, edge paths, geometry and distance.
- `gaps` lists the breaks. Points that do not snap, time gaps that are too
  large and missing legal transitions split the sequence. Isolated points
  stay unmatched.

`confidence` is `exp(-score / (2 * observation_count - 1))`. It is an
uncalibrated model-fit score. It is not the probability of the correct road.
It does not quantify ambiguity between competing roads. `confidence_method`
states this in each result. Lower residual and transition penalties decrease
`score` and increase `confidence`.

`?format=geojson` returns the matching geometries as features. It also
returns trace points and gaps that align with the input. A stationary
matching can be a Point. Feature properties keep the profile identity,
observation indexes, distance and score interpretation.

## Limits

The matcher does not support:

- calibrated ambiguity estimates;
- high throughput on large traces;
- time-dependent traffic costs;
- directions for the matched path;
- the richer edge attributes of
  [Valhalla's trace services](https://valhalla.github.io/valhalla/api/map-matching/).

## Related pages

- [Snap constraints](snap-constraints.md).
- [Routing](routing.md).
- [HTTP API reference](../api/reference.md) for the full schema.
