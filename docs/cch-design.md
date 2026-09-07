# CCH routing accelerator

NetWeevil uses an edge-based Customizable Contraction Hierarchy. Dataset
preprocessing depends on legal transitions; profile compilation customizes
weights without rebuilding the hierarchy.

## Dataset build

Hierarchy vertices are directed road-edge states. Its base arcs are legal
edge-to-edge transitions, including permitted u-turns.

Nested dissection partitions the undirected edge-transition graph. Recursive
partitions place separator states last; minimum-degree elimination orders
small leaves. The directed elimination game then contracts states in rank
order. Contracting `v` inserts `x -> y` for every `x -> v -> y` whose endpoints
outrank `v`, excluding `x == y`. It deduplicates arcs without an insertion
budget. Keeping every required shortcut preserves shortest paths under any
supported nonnegative profile metric.

The dataset bundle stores edge order and rank, and upward and downward CSR
adjacency sorted by head id. It stores no expanded shortcut paths.

## Profile customization

Base arcs receive the cost of the head edge plus the transition penalty.
Forbidden transitions and shortcuts without a reachable decomposition start
at infinity. Basic customization processes states in increasing rank and
relaxes each lower triangle, `w(x -> y) = min(w(x -> y), w(x -> v) + w(v -> y))`.
Both child weights are final before their triangle is visited.

Each arc has three `u32` weights for generalized cost, travel time, and
distance, totaling 12 bytes. This figure excludes dataset adjacency and
per-edge profile data. A weight unit is 1/1024 of the metric unit. Base
transition costs round to the nearest unit. Infinity and finite overflow have
distinct sentinel values; overflow cannot turn a reachable route into an
unreachable one. Metrics that cannot be represented use exact routing.

For a path with `n` transitions, the rounding error is at most `n/2048`
metric units. Quantization can change the selected route when alternatives
are close. If the selected and exact-optimal paths contain `n` and `m`
transitions, the selected path's excess original cost is bounded by
`(n + m)/2048`, with endpoint costs held fixed. Public route summaries compute
costs from the reconstructed road edges and original profile metrics.
Service-area labels inherit the same error bound, so threshold membership can
differ for edges within that bound of the cutoff.

## Queries and reconstruction

Forward search follows upward arcs from origin seeds. Backward search follows
reversed downward arcs from destination seeds. Each direction stops when its
own queue minimum reaches the best complete cost. A sum of the two queue
minima is not a valid stopping criterion because the searches use different
arc sets. Snapped edge-interior endpoints enter as partial-edge seeds.

Reconstruction finds a lower triangle whose integer child weights sum to the
parent weight, then expands its children. Decreasing middle rank guarantees
termination, including for zero-cost transitions. An arc without a matching
lower triangle unpacks to a base transition. No middle-state arrays or full
shortcut expansions are persisted.

The exact engine is the differential-test reference and handles metric
overflow. A hierarchy query optimizes the quantized metric. The engine checks
its candidate against multi-edge restrictions and uses an exact automaton
search when the candidate violates one. A legal candidate needs no exact
follow-up search.

## Bundle validation

Readers require the current bundle format and schema. Unsupported dataset
bundles fail with a re-import instruction. Compiled profiles are validated
against the dataset acceleration bundle id and recompile when stale.
