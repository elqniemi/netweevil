# Future improvements

This page lists improvements that the maintainers plan. The list has no
order of priority and no dates. To work on an item, open an issue first.

## Testing

- **Router-level HTTP tests.** Add a test for each API route with
  `tower::ServiceExt::oneshot`. Each test checks the status codes,
  `?format=geojson`, the error shape and the HTTP 503 response in setup mode.
- **One check script.** Add a script that runs all checks in one step:
    - the Rust format, clippy and tests,
    - the web console typecheck and tests,
    - the QGIS plugin package, `py_compile` and `ruff`,
    - the `unittest` suite of the scripts.
- **Fixture validation.** Add a test that parses each
  `examples/**/*.json` request into its request type. Add a test that
  validates each profile in `examples/profiles/*.yml`.
- **End-to-end smoke test.** Commit a small synthetic OSM PBF file and a small
  GTFS feed. Use them in a test to import, compile, serve and send requests with
  `curl`.
- **Pre-publish lint.** Add a check that uses `git grep` to find email
  addresses, home folder paths and old identifiers.

## API design

- **Uniform JSON errors.** Use a custom axum rejection, so that 400, 415 and
  422 errors are JSON and not `text/plain`.
- **Status codes for analysis failures.** Return 422 when an analysis fails,
  for example an unreachable destination or a point that does not snap. Keep
  400 for a malformed request.
- **Setup mode first.** Check for setup mode before the API parses the request
  body.
- **One `format` parameter rule.** Make the `format` parameter case
  consistent. Send `application/geo+json` for all GeoJSON results. Remove the
  ignored `format=geojson` from endpoints that give only JSON.
- **Consistent status codes.**
    - Return 201 or 202 for create, upload, import and activate operations.
    - Use one response shape for all delete operations.
    - Return 422, not 404, when a dataset has no DEM.
    - Return 404 for an unknown access or egress profile, the same as for an
      unknown feed.
- **Strict request fields.** Add `deny_unknown_fields` to nested request
  types, so that a misspelled field causes an error.
- **Generated OpenAPI document.** Optional: generate the OpenAPI document from
  the code (for example with `utoipa`). This removes the manual
  synchronization of the document and the router.
- **One ordering API.** Replace `optimize_order` of `/v1/waypoints` with
  `fixed_endpoints` of `/v1/trip`.
- **One service endpoint.** Merge `/v1/profiles` into `/v1/service`. The two
  endpoints give the same profile data.

## Routing and transit

- **Same transit street access in the CLI and the API.** Use one street-access
  estimator for the CLI and the API. Then `analyze transit-route` supports
  bicycle and car access, the same as `POST /v1/transit-route`.

## Operations and security

- **API hardening.**
    - Make the CORS origin configurable. The API now sends permissive CORS
      headers.
    - Add a size limit for uploads.
    - Add an optional bearer token.
- **Persistent background jobs.** Save the background job records of
  `crates/api/src/jobs.rs`, or mark them as failed after a restart. The API now
  keeps them in memory only.

## Documentation and data

- **Purpose-based profile names.** Rename the example profiles that have a
  date or research name, for example `car_research_v3` and
  `*_north_nl_2026_05`. Give them names that tell their purpose.
