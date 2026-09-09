# Changelog

## Unreleased

- Road snapping uses a prepared edge index, including the interiors of long
  edges. Snap caches preserve point IDs and full coordinate precision.
- CCH and restriction-aware Dijkstra stopping account for partial-edge
  destination costs. Matrix searches validate restrictions and reuse an exact
  frontier across destinations from the same origin.
- `POST /v1/locate` exposes profile-aware, directional snap candidates.
- Transit distinguishes each dated/frequency vehicle run, enforces scheduled
  pickup/drop-off rules, and preserves repeated-stop connection sequences.
  Geometric transfers cover the complete requested radius and honor transfer
  slack in both directions. Transit bundles require re-importing.
- Transit departure indexes use connection references and continuation links.
- Benchmarking supports Valhalla alongside NetWeevil and OSRM. Reports
  fingerprint selected corpus inputs and require the current schema.
- OD and point-set files use canonical longitude/latitude columns and object
  documents. Report exports and the QGIS plugin use the same names.
- Unused helpers, compatibility readers, settings migrations and manifest
  reexports have been removed.

See [routing parity](docs/routing-parity.md) for measurements, verification,
remaining feature gaps and implementation order.
