# Bearing and road-side constraints

Snapping attaches each input point to a directed road. Use snap constraints
to require a travel direction or a side of the road at a point. An example is
a pickup on the curb side of the street.

Routes, directions, matrices, location inspection, GPS trace matching and
waypoints accept `snap.point_constraints`. The keys are the `id` values of the
input points. A point without an entry snaps without restriction.

## Prerequisites

- An imported dataset and a compiled profile.
- Input points with unique `id` values.

## Add a constraint

1. Find the `id` of the point that needs a constraint.
2. Add an entry for that `id` under `snap.point_constraints`.
3. Set `bearing`, `approach`, or both.

```json
{
  "max_distance_m": 100,
  "point_constraints": {
    "pickup": {
      "bearing": {"degrees": 90, "tolerance_degrees": 20},
      "approach": "curb",
      "driving_side": "right",
      "street_side_tolerance_m": 5
    },
    "dropoff": {
      "approach": "opposite",
      "driving_side": "right"
    }
  }
}
```

The object above is the value of `snap` inside a request.

=== "CLI"

    Put the `snap` object in the request file of `analyze route`,
    `analyze directions` or `analyze matrix`.

=== "HTTP"

    Put the `snap` object in the request of `/v1/route`, `/v1/directions`,
    `/v1/matrix`, `/v1/locate`, `/v1/match` or `/v1/waypoints`.

    ```bash
    curl -X POST http://127.0.0.1:8080/v1/locate \
      -H 'Content-Type: application/json' \
      --data @examples/api/locate_constraints.json
    ```

## Constraint fields

| Field | Default | Meaning |
| --- | --- | --- |
| `bearing.degrees` | required with `bearing` | Travel direction clockwise from north, from 0 to 360 |
| `bearing.tolerance_degrees` | required with `bearing` | Largest circular angular difference, from 0 to 180 |
| `approach` | `unrestricted` | `unrestricted`, `curb` or `opposite` |
| `driving_side` | country of the edge | `left` or `right` |
| `street_side_tolerance_m` | 5 | Points this close to the center line have no side |

### Bearing

Bearings wrap at north: 359 degrees and 1 degree differ by two degrees. The
direction follows the local tangent of the directed routing segment. The
engine rejects non-finite values and values outside the ranges.

### Approach

- With `driving_side: right`, `curb` keeps roads where the point is to the
  right of travel. `opposite` keeps roads where the point is to the left.
- `driving_side: left` reverses those choices.
- Without `driving_side`, each candidate edge uses the traffic side of its
  country from the admin areas of the dataset. Where the country is unknown,
  the engine uses right-hand traffic. See
  [Regions and elevation](regions-and-elevation.md#administrative-areas).

Points within `street_side_tolerance_m` of the center line have no side. For
them, either direction stays eligible if its bearing matches. The tolerance
is a finite, non-negative distance in metres. At intersections, the engine
applies the same test to the directed tangent of each candidate segment.

When you set both a bearing and an approach, a candidate must match both.

## Behavior

Constraints filter the directed roads that the selected profile allows. They
do not create reverse travel on one-way roads. They do not override elevation
and attribute filters. They are strict filters, not routing preferences.

- A constrained intersection snap keeps an edge id and a fraction, including
  fractions 0 and 1.
- An origin must depart on the eligible edge. A destination must arrive on
  it. A zero-length traversal cannot satisfy the direction of the opposite
  endpoint.
- Through waypoints keep their turn history. Break waypoints apply the
  constraint to both arrival and departure.
- Stop ordering uses the same directed costs.

!!! warning
    A constrained request can be unreachable even when an unrestricted route
    exists.

### Matrices

For a matrix, each point set supplies its own `snap` object. If an id occurs
in both sets, its constraints must agree. Use different ids for different
requirements. The engine rejects conflicts in static and temporal matrices.

## Background

The angle conventions and curb semantics follow the concepts of the
[OSRM general options](https://project-osrm.org/docs/v5.24.0/api/#general-options).
The driving-side choice and center-line tolerance are comparable to the
[Valhalla location options](https://valhalla.github.io/valhalla/api/route/api-reference/).
The request schema is specific to NetWeevil.

## Related pages

- [Routing](routing.md#inspect-routing-locations).
- [Trace matching](trace-matching.md).
- [HTTP API reference](../api/reference.md) for the full schema.
