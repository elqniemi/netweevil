# GTFS transfer rules

`transfers.txt` is imported into transit bundle schema 6. Re-import existing feeds after upgrading. Changes to the file contribute to the source hash and invalidate derived bundles.

Depart-at, arrive-by, and service-area searches apply these rules when changing vehicles:

- Type 0 marks a recommended connection. Normal walking and boarding feasibility still apply.
- Type 1 marks a timed connection. NetWeevil uses scheduled departure times and does not simulate a vehicle waiting for another vehicle. Import output and every affected feed's query result include this limitation in diagnostics.
- Type 2 requires `min_transfer_time`. The elapsed gap between arrival on the incoming vehicle and departure on the outgoing vehicle must meet that minimum.
- Type 3 prohibits the connection.

A supplied minimum is measured across the whole interchange, including its walking time. Request walking limits, boarding slack, and transfer slack must also be satisfied. A larger minimum replaces the smaller feasibility threshold; it is not added a second time to the walking duration. Rules do not create a street path when an explicitly supplied network transfer table has none.

Rules can select stops, routes, or trips. A station selector applies to all its child stops. The strongest matching rule wins, following the six levels in the GTFS reference: two trips; one trip and the opposite route; one trip; two routes; one route; stops alone. A trip selector takes precedence over its own route selector, whose membership is validated when both are supplied.

Duplicate normalized scopes are rejected during import. If distinct rules of equal highest specificity both match a queried trip pair, the query returns an error instead of choosing by input order. Use a more specific rule to resolve the conflict. Trip-specific rules for services outside the imported service window are omitted.

The router retains the incoming and outgoing vehicle connection across a transfer walk, so a faster but forbidden interchange cannot discard a slower legal one. Consecutive walking hops cannot erase the rule context. Staying aboard the next connection of the same vehicle run does not count as a transfer and is not blocked by an interchange prohibition.

Linked-trip types 4 and 5 are rejected during import. Cross-trip in-seat continuity, coupling, and vehicle holding remain unsupported. Ordinary continuation within one GTFS trip is supported.

These semantics follow the [GTFS transfers reference](https://gtfs.org/documentation/schedule/reference/#transferstxt). Type 1's scheduled-only interpretation is an explicit limitation, not an implementation of its vehicle-holding guarantee.
