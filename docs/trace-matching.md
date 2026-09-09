# GPS trace matching

`POST /v1/match` matches a sequence of observations to a prepared street profile.
It uses Gaussian positional-error penalties and the difference between observed
displacement and shortest legal network distance. Viterbi states retain turn
restriction history across observations, including multi-edge restrictions.
The transition search minimizes distance in metres over edges allowed by the
profile. It does not minimize the profile's time or generalized cost.

```bash
curl -X POST http://127.0.0.1:8080/v1/match \
  -H 'content-type: application/json' \
  --data @examples/api/north_nl_match.json
```

The payload uses `{profile_id?, request}`. Each observation contains a `point`
with `id`, `lon`, `lat` and optional `z`. An optional `accuracy_m` overrides
the request's positional-error standard deviation. `snap` accepts the same
elevation, attribute, bearing and side-of-road constraints as other analyses.

| Setting | Default | Meaning |
| --- | ---: | --- |
| `snap.max_distance_m` | 50 | Candidate search radius in metres |
| `max_candidates` | 8 | Maximum directed candidates per observation, from 1 to 8 |
| `gps_accuracy_m` | 10 | Positional-error standard deviation in metres |
| `transition_beta_m` | 50 | Scale of the network-distance discrepancy penalty |
| `max_transition_distance_m` | 20,000 | Maximum legal distance between adjacent matched observations |
| `max_time_gap_s` | 60 | Split a timestamped trace across larger gaps |
| `max_speed_mps` | 70 | Additional distance bound from elapsed timestamp seconds |
| `max_transition_states` | 100,000 | Hard search limit per origin candidate and restriction state |
| `max_search_states` | 2,000,000 | Hard total search-state budget across the entire request |

Supply between 2 and 512 observations. Timestamps must be finite, strictly
increasing and supplied for every observation or none. They can use any common
seconds-based time origin. They constrain continuity and speed; temporal road
costing is not supported. Exceeding the explicit search-state limit fails the
request instead of returning an incompletely searched optimum.

The response is `{service, result}`. `result.tracepoints` is aligned with the
input and contains matched network positions or null for an unmatched point.
`matchings` contains connected sequences with observation indexes, edge paths,
geometry and distance. Unsnappable points, excessive time gaps and absent legal
transitions split the sequence and produce explicit `gaps`. Isolated points
remain unmatched.

`confidence` is `exp(-score / (2 * observation_count - 1))`, an uncalibrated
model-fit score. It is not a probability of choosing the correct road and does
not quantify competing-road ambiguity. `confidence_method` states this in
every result. Lower residual and transition penalties decrease `score` and
increase `confidence`.

`?format=geojson` returns matching geometries as features, plus input-aligned
trace points and gaps. A stationary matching may be a Point. Feature properties
retain profile identity, observation indexes, distance and score interpretation.

Remaining work includes calibrated ambiguity estimates, large-trace throughput,
time-dependent traffic costs, matched-path directions and the richer edge
attributes exposed by [Valhalla's trace services](https://valhalla.github.io/valhalla/api/map-matching/).
