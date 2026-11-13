# Indoor and surface routing

The `netweevil-indoor` crate routes on walkable surfaces and on node/edge
networks, indoors and outdoors, across levels. The CLI and the API use it
for indoor sites: see the [indoor routing guide](../guides/indoor-routing.md).

The engine has two phases:

1.  **Compile.** `compile(&Input)` turns an input document into one graph.
    The graph can be saved to a binary file and loaded again.
2.  **Query.** A `Router` holds the graph and a cache of compiled profiles.
    A query locates its two ends, attaches them to the graph and runs an A*
    search. Then it draws the route geometry.

## Model

| Element | Meaning |
| ------- | ------- |
| Level | A floor or an outdoor layer. Each level has an id and a height `z`. |
| Surface | A walkable polygon on a level: a room, a corridor, a plaza. Routes cross surfaces at any angle. |
| Portal | An opening in a wall or a barrier: a door, a gate, a crossing. |
| Barrier | A line that cuts a surface: a fence, a kerb. Only portals pass it. |
| Edge | A line of a network: a footway, a road, a single indoor line. |
| Node | A named point of the network. Edges that share a node join there. |
| Vertical | A lift, stairs, an escalator or a ramp. It joins stops on different levels. |
| POI | A destination with one or more access points. |

Coordinates are projected metres. A vertex is `[x, y]` or `[x, y, z]`. When
`z` is not given, the engine uses the `z` of the element, then the `z` of the
level, then `0`.

### Surfaces and the navigation mesh

The engine triangulates the surfaces and barriers of each level into one mesh.
Two surfaces that share a boundary are connected when both have
`walls: false`, or when both belong to the same `space`. Otherwise the shared
boundary is a wall, and only portals pass it. A **zone** is a connected part
of the mesh.

A walk inside a zone is a taut path through its triangles (a funnel). The
drawn line keeps a clearance from the walls and has round, square or sharp
corners, as the profile sets. The drawn line never leaves walkable space.

### The graph

The graph has one node for each portal side, each edge junction, each
vertical stop and each POI access point. Its arcs are:

- walks between the nodes of one zone,
- edge pieces between edge nodes,
- portal crossings,
- vertical hops,
- connectors from an edge end to the surface it lies on.

Input with only edges routes along the edges. Input with only surfaces routes
on the mesh. Mixed input does both.

## Input schema

An input document is JSON. Unknown fields are ignored. Each `id` is optional.
A missing id becomes `"{kind}#{index}"`. Each element can have `tags`
(strings that profiles use) and `props` (any JSON that the route output
returns unchanged).

```jsonc
{
  "version": 1,
  "levels": [{ "id": "L0", "z": 0.0, "props": {} }],
  "defaults": { "snap": 0.05, "edge_attach": 1.0, "poi_snap": 30.0 },
  "surfaces": [{
    "id": "room_12", "level": "L0",
    "polygon": [[[0, 0], [10, 0], [10, 8], [0, 8], [0, 0]]],
    "space": "room_12", "class": "room", "walls": true,
    "cost_factor": 1.0, "speed": null, "tags": ["indoor"]
  }],
  "edges": [{
    "id": "e1", "line": [[10, 4], [40, 4]], "level": "L0",
    "from": "n1", "to": "n2", "oneway": false, "class": "footway",
    "speed": null, "time": null, "cost_factor": 1.0, "penalty": 0.0,
    "width": null, "step_free": true, "incline": null
  }],
  "nodes": [{ "id": "n1", "at": [10, 4], "level": "L0", "penalty": 0.0 }],
  "portals": [{
    "id": "d1", "kind": "door", "segment": [[10, 3], [10, 5]], "level": "L0",
    "spaces": ["room_12", "corridor_1"], "penalty": 2.0, "oneway": null,
    "width": 0.9, "step_free": true
  }],
  "barriers": [{ "id": "b1", "line": [[0, 20], [40, 20]], "level": "L0" }],
  "verticals": [{
    "id": "lift_A", "kind": "lift",
    "stops": [{ "level": "L0", "at": [5, 5] }, { "level": "L1", "at": [5, 5] }],
    "wait": 30.0, "speed": 1.0, "direction": "both", "step_free": true
  }],
  "pois": [{
    "id": "wc_1", "category": "toilet/accessible",
    "access": [{ "portal": "d1" }, { "at": [3, 3], "level": "L0" }],
    "tags": ["wheelchair"]
  }]
}
```

Notes:

- A surface has `polygon` (with holes) or `multipolygon`. `speed` sets a
  fixed speed, for example on a travelator.
- An edge end within `edge_attach` metres of a surface on its level attaches
  to that surface. Edges join at shared vertices (within `snap`) and at
  shared `from` and `to` node ids. `time` sets fixed seconds for the edge.
- A portal is a `segment`, or an `at` point with a `width`. `oneway` is
  `null`, `"forward"` (along the normal) or `"backward"`.
- A vertical `direction` is `both`, `up` or `down`. `line` can give the 3D
  walking line of stairs.
- A POI access is a point (`at`, `level`), a `node`, a `portal` (both sides)
  or a `space` (its representative point). A point off every surface snaps to
  the nearest edge within `poi_snap` metres.
- `access_groups` on any element restricts it to queries that are granted
  at least one of the groups. The default is public.

Problems in the input, such as a portal with no wall to sit in, are compile
warnings. They do not stop the compile.

## Profiles

A profile sets the costs of one kind of traveller. The built-in profiles are
`walk`, `wheelchair`, `fastest`, `bike` and `evacuation`.

| Field | Meaning |
| ----- | ------- |
| `speed` | Speed on flat ground, in m/s. |
| `class_factor`, `tag_factor`, `kind_factor` | Cost factors by class, tag and kind. `null` forbids. |
| `kind_factor_up`, `kind_factor_down` | Factors by vertical kind or edge class when going up or down. |
| `avoid_tags`, `require_tags` | Elements to avoid, and tags that surfaces and edges must have. |
| `step_free`, `min_width` | Forbid elements with steps, or narrower than the width. |
| `incline` | Extra cost uphill and downhill, and the maximum grade. |
| `kind_penalty`, `tag_penalty` | Seconds added for each traversal. `tag_penalty` adds to the cost, not to the duration. |
| `tag_speed` | Speed caps by tag, class or kind. |
| `turn_penalty` | Seconds for each radian of heading change at graph nodes. |
| `clearance`, `clearance_max`, `corner`, `corner_radius` | How the walks are drawn. |
| `on_foot`, `leave_tags` | Mixed travel: where the profile may not go, it walks with this profile. |
| `mode` | The element kinds that a route can use (see below). |

### Routing modes

The `mode` field of a profile limits the element kinds that a route uses.

| Mode | Route uses | Point locators snap to |
| ---- | ---------- | ---------------------- |
| `mixed` (default) | Surfaces, edges, portals, verticals and nodes. | Surfaces and edges. |
| `edges_only` | Edges, portals, verticals and nodes. A route never walks across a surface. | Edges. A `space` locator fails. |
| `surfaces_only` | Surfaces, portals, verticals and nodes. A route never travels along an edge. | Surfaces. |

The mode gives the excluded elements an infinite cost factor, as a `null`
class factor does. The default mode adds no work to a query. When an end of a
query cannot be located in the mode, the query fails with an error that names
the mode.

## Queries

A locator (`Loc`) is one of these:

- a point: `at` with an optional `level`,
- a `space`, optionally with `at` for a spot in that space,
- a `node` or a `portal`,
- a `poi`, a list of `pois`, or a `category` with optional `tags` and
  `exclude`. The route ends at the best access point.

The `Router` gives these queries:

| Query | Result |
| ----- | ------ |
| `route` | A `Route`: cost, duration, length, climb, coordinates and one segment for each walk, edge, portal or vertical. |
| `costs_to`, `costs_from` | The cost, duration and length between one point and many points. `costs_to` is one search. |
| `route_stops` | A trip through via stops, in the given order or in the cheapest order. |
| `route_closed` | A route with closures and waits in force. |
| `locate` | Where a locator snaps. |
| `reach` | Connectivity: components and unreachable spaces, portals, edges and POIs. |
| `elements`, `levels` | The portals, nodes, verticals and POIs with their graph positions, and the levels with `z`. |

The `cost` of `costs_to` is the same as the `cost` of `route`. Its duration
and length are those of the taut path. `route` draws walks with clearance
and round corners, so its duration and length can be a few percent larger.
