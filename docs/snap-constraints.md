# Bearing and road-side constraints

Routes, directions, matrices, location inspection, GPS observations and waypoints
use `snap.point_constraints`, keyed by each input point's `id`. An omitted entry
keeps the usual unrestricted snapping behavior. Constraints filter the directed
roads eligible under the selected profile; they do not create reverse travel on
one-way roads or override elevation and attribute filters.

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

The object above is the value of `snap`, inside an API request or a query document.
For a matrix, each point set supplies its own `snap` object. If an ID occurs in
both sets, its constraints must agree; use distinct IDs for different requirements.
Conflicts are rejected in both static and temporal matrices.

`bearing.degrees` is the direction of travel clockwise from north, from 0 through
360 degrees. `tolerance_degrees` is the maximum circular angular difference, from
0 through 180 degrees. Both values are required when `bearing` is supplied.
Bearings wrap at north: 359 degrees and 1 degree differ by two degrees. Nonfinite
values and values outside these ranges are rejected. The direction follows the
local tangent of the directed routing segment.

`approach` accepts `unrestricted` (default), `curb`, or `opposite`. With
`driving_side: right`, curb approach keeps roads where the input point lies to the
right of travel; opposite approach keeps roads where it lies to the left.
`driving_side: left` reverses those choices. The caller supplies the driving side;
NetWeevil does not infer it from OSM or country boundaries. The default is right.
These are strict candidate filters, rather than routing preferences.

Points within `street_side_tolerance_m` of the road's center line have no side,
so either direction remains eligible if its bearing matches. The tolerance is a
finite nonnegative distance in metres, defaulting to 5. At intersections the
same test uses each candidate segment's directed tangent. Combining a bearing
and approach requires both to match.

A constrained intersection snap retains an edge ID and fraction, including
fractions 0 and 1. Origins must depart on the eligible edge and destinations must
arrive on it; a zero-length traversal cannot satisfy the opposite endpoint's
direction. Through waypoints retain their turn history, and break waypoints apply
the constraint to both arrival and departure. Stop ordering uses these same
directed costs. A request can therefore become unreachable even when an
unrestricted route exists.

The angle conventions and curb semantics follow the concepts described in the
[OSRM general options](https://project-osrm.org/docs/v5.24.0/api/#general-options).
The explicit driving-side choice and center-line tolerance are comparable to
[Valhalla location options](https://valhalla.github.io/valhalla/api/route/api-reference/).
The request schema here is NetWeevil's own.
