# Incremental dataset updates

`netweevil dataset update` applies OSM change files to an imported OSM PBF
dataset. It produces the topology a full import of the changed extract
produces and reuses the CCH elimination order, so it skips the
nested-dissection ordering that dominates import time.

## Source snapshot

An OSM import reads the extract in one pass and writes a source snapshot
(`.netweevil/bundles/source`, `NWSECB06`) next to the other bundles:

- every node position, except that topology nodes store only their OSM id
  because the topology bundle already holds their coordinates;
- the routable ways as classified records (id, node list and name are
  separate columns, the rest is interned);
- the turn-restriction relations and the traffic-signal node ids;
- the ids of the other ways and relations, which keep the source object
  counts exact.

A change file only lists the objects it touches. A modified way can reference
an unmodified node that no routable way used before, so the snapshot keeps
every node position. Classification is not stored as raw tags: changed
objects arrive with all their tags, and a classifier change requires a full
re-import.

| Extract | PBF | Snapshot | Topology | Acceleration |
| --- | ---: | ---: | ---: | ---: |
| Groningen routing extract | 6.85 MB | 2.09 MB | 6.21 MB | 8.59 MB |
| Groningen, unfiltered | 56.1 MB | 25.4 MB | 6.22 MB | 8.59 MB |
| North Netherlands routing extract | 26.9 MB | 8.14 MB | 26.5 MB | 36.4 MB |

The unfiltered extract shows why the routing filter in the README matters:
the snapshot holds positions of every building node.

## Applying changes

The update restores the source state from the snapshot and the previous
topology, applies each change file in order (deletions remove the object;
creations and modifications replace it with the newest version in the file,
as `osmium apply-changes` does), and rebuilds the topology with the import
code. `--source new.osm.pbf` scans a replacement extract instead.

The topology is therefore identical to a full import of `osmium apply-changes
base.osm.pbf changes...`: applying 14 consecutive Geofabrik daily diffs
(sequences 2735 to 2748) to the Groningen routing extract one at a time gives
a topology bundle whose sections after the source header, the edge names,
the topology metadata and the source snapshot are byte-identical to the full
import of the osmium-updated extract.

## Reusing the CCH order

The elimination order depends only on the topology. Edge states keep their
previous rank when their identity survives: source way, OSM tail and head
node, and occurrence, falling back to an unclaimed state over the same node
pair (way splits and merges). A new state is placed directly after its
highest-ranked neighbour that already has a place, so a state adjacent to
both sides of a separator lands in that separator and one inside a cell stays
there. States reachable only through other new states are placed in
breadth-first layers; components with no reused state are ordered by nested
dissection and go first.

Contraction inserts every required shortcut for any order (see the
[CCH design](cch-design.md)), so the hierarchy stays complete and queries
stay exact. Only the shortcut count depends on the order.

Each update records fill growth: the shortcut count divided by the count the
last full ordering's shortcut density predicts for the current edge count.
`--reorder auto` (default) reorders by nested dissection when fill growth
exceeds `--max-fill-growth` (default 1.10), `--reorder always` reorders
unconditionally and `--reorder never` always patches.

| Groningen | Patched shortcuts | Fresh shortcuts | Excess | Recorded fill growth |
| --- | ---: | ---: | ---: | ---: |
| 1 real daily diff | 5,311,329 | 5,308,339 | 0.06% | 1.0004 |
| 14 real daily diffs, applied one by one | 5,344,519 | 5,311,042 | 0.63% | 1.0061 |
| 944 synthetic edits in one change file | 5,302,643 | 5,282,101 | 0.39% | 0.9998 |

The recorded fill growth tracks the real excess for edits that add roads;
deletions lower the fresh count as well, so the synthetic day with many
closures underestimates it.

## Equivalence tests

`crates/ingest/src/update/tests.rs` imports a base extract and the
independently edited extract (written as PBF), applies the change file to the
base, and checks:

- the topology bundle, edge names, topology metadata and source snapshot
  equal the full import's;
- every sampled route has the same distance, time and generalized cost on the
  updated CCH engine, the full import's exact engine and its CCH engine;
- a replacement extract with `--reorder always` reproduces the full import's
  elimination order and shortcut count, and chained change files reach the
  same source hash as one call with all files.

The synthetic edits cover tag and speed changes, a closed road, a new road
over new nodes, a moved node, a way split, a deleted node, a new signal, a
new and a removed turn restriction, and a building turned into a road over
nodes that were not in the topology. The ingest grid completeness test also
runs on a patched order. `extract_update_matches_a_full_import` (ignored)
repeats the comparison on a real extract with 944 generated edits; on
Groningen all 705 sampled routes agree, 687 of them routable.

## Timings

Measured on a shared 10-core Apple M1 Max with other builds running, so
times are noisy (the Groningen import took 15 to 45 s across runs).

| Dataset | Full import | Update | Load | Apply | Topology | Patch order | Contract | Write |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Groningen, 1 daily diff | 15 to 18 s | 1.8 s | 0.09 s | 0.03 s | 0.59 s | 0.15 s | 0.56 s | 0.38 s |
| North NL, 3 province diffs | 209 s | 8.0 s | 0.37 s | 0.15 s | 2.85 s | 0.76 s | 2.38 s | 1.48 s |

North NL here is the Geofabrik Groningen, Friesland and Drenthe extracts
merged and filtered as the README describes: 2,167,461 nodes, 4,514,489
directed edges and 22,607,243 shortcuts. Its update applied the provinces'
latest daily diffs, which the extracts already contain, so it measures the
full update path without topology changes. Peak RSS was 2.87 GB for the
import and 3.55 GB for the update, which holds the previous and the new
build at once.

Through the API, `POST /v1/workspace/datasets/groningen/update` with two
daily diffs finished, including the reload and profile recompile, in 4.1 s
while route requests kept being served by the previous runtime.
