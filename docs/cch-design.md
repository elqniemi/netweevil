# CCH Routing Accelerator — Design

Netweevil's primary route accelerator is an edge-based Customizable Contraction
Hierarchy (CCH). The dataset-level preprocessing is metric independent; each
compiled profile customizes arc weights without rebuilding topology.

## Why the previous accelerator was replaced

The previous "shortcut CH" build (`edge_based_shortcut_ch_v1`) capped shortcut
insertion with budgets (`max_shortcut_budget_per_edge`,
`max_shortcuts_per_contracted_edge`, `max_shortcut_path_len`). A capped
elimination is not a valid hierarchy: queries over it return upper bounds only,
so every route still had to run a full exact search seeded with that bound.
Customization also walked each arc's frozen full base-edge path, which is slow
and does not yield the minimum over decompositions under the active profile.

## Structure

Vertices of the hierarchy are directed edge states (edge-based routing), arcs
are legal edge-to-edge transitions plus shortcuts.

### Dataset build (metric independent, once per dataset)

1. Order edge states by recursive geometric bisection (nested-dissection
   style): recurse on halves, emit separator states last. Rank = position in
   this order.
2. Elimination game, processing states in rank order, with NO budgets:
   contracting state `v` adds shortcut `x -> y` for every incoming arc
   `x -> v` and outgoing arc `v -> y` with `rank(x), rank(y) > rank(v)`,
   `x != y`, deduplicated by `(tail, head)`. Completeness of this step is what
   makes the hierarchy query exact on its own.
3. Persist (schema v3, algorithm `edge_based_cch_v2`): `edge_order`,
   `edge_rank`, upward CSR (`tail rank < head rank`) and downward CSR
   (`tail rank > head rank`), adjacency sorted by head id for binary-search
   lookup. No path expansions are stored.

### Profile customization (per compiled profile)

1. Initialize: arcs that correspond to real base transitions get the
   turn-adjusted transition cost (head-edge generalized cost + turn penalty,
   `inf` when forbidden); pure shortcuts start at `inf`.
2. Basic customization, replaying the elimination order: for each state `v` in
   rank order, for each arc pair (`x -> v`, `v -> y`) with higher-ranked
   `x, y`: relax `w(x -> y) <= w(x -> v) + w(v -> y)`. When a relaxation
   improves an arc, record `middle = v` for unpacking.
3. Persist per-profile `upward_weight`, `downward_weight`, `upward_middle`,
   `downward_middle` (sentinel `u32::MAX` = base transition).

Processing middles in increasing rank guarantees both child arcs have final
weights before any triangle that uses them is relaxed.

### Query

Bidirectional search: forward relaxes upward arcs from the origin seeds,
backward relaxes downward arcs toward the destination seeds (via the reverse
downward CSR). Termination is per direction: a direction stops only when its
queue minimum is `>= best`. The sum rule (`topF + topB >= best`) is NOT valid
for hierarchy searches because the two directions explore different arc sets,
so a meeting arc is never relaxed from both sides.

Snapped endpoints in edge interiors enter as seeds with partial-edge costs.
Path unpacking is recursive through `middle` pointers down to base
transitions; child arcs are located by binary search in the sorted CSR rows.

The CCH result is authoritative: no exact re-search runs after it. The exact
engine remains the differential-test oracle and the fallback when a profile
activates multi-edge restriction automata (which the CCH does not model) or
when a dataset has no acceleration bundle.

## Compatibility

Datasets imported with the previous algorithm fail to load with a clear
"re-import this dataset" error, consistent with previous bundle format
migrations. Compiled profiles are validated against the dataset acceleration
bundle id and recompile automatically when stale.
