# Reports and experiments

Each CLI analysis writes a run manifest next to its result. This guide shows
how to run a study file with several analyses, and how to render a run
manifest as a report.

## Prerequisites

- An imported dataset and compiled profiles.
- For a report: a run manifest from an analysis or simulation run.

## Reproducibility

The engine is deterministic. A result depends on these inputs:

- the dataset source extract (and any boundaries file and DEM of the import);
- the profile document;
- the request.

To reproduce a run, keep those inputs. Each CLI run manifest stores the
merged request under `effective_request`, after command-line overrides. Thus
you do not need to change the source request file to record an override.

## Run manifests

A run manifest is a JSON file in `.netweevil/runs/`. It contains:

| Field | Meaning |
| --- | --- |
| `run_id`, `run_kind`, `status`, `created_at` | Identity and state of the run |
| `dataset_id`, `profile_id`, `compiled_profile_bundle_id` | Inputs |
| `request_source` | The request file |
| `effective_request` | The resolved request after CLI overrides |
| `result_path`, `report_path` | Output files |
| `software`, `algorithm`, `methods_summary` | Software version and method description |
| `connectivity_policy`, `fallback_policy` | Policies of the run |
| `message` | Status message |

`run_kind` is one of `route`, `directions`, `route_batch`, `od`, `matrix`,
`accessibility`, `service_area`, `service_area_sequence`, `betweenness`,
`scenario_batch`, `experiment`, `simulation` or `trip`.

## Run an experiment

A study file lists scenarios. Each scenario runs one analysis with one
profile.

1. Write a study file (YAML or TOML). Paths in the file are relative to the
   file.

    ```yaml
    experiment:
      id: baseline_sweep
      label: Baseline research sweep
      dataset: groningen_2026_05

    scenarios:
      - id: baseline_route
        profile: ../profiles/car_research_v1.yml
        analysis: route
        request: ../requests/route.json
        out: ../../.netweevil/runs/experiment-baseline-route.geojson

      - id: baseline_od
        profile: ../profiles/car_research_v1.yml
        analysis: od
        request: ../requests/od_pairs.csv
        out: ../../.netweevil/runs/experiment-baseline-od.csv

      - id: baseline_matrix
        profile: ../profiles/car_research_v1.yml
        analysis: matrix
        origins: ../requests/matrix_origins.csv
        destinations: ../requests/matrix_destinations.csv
        out: ../../.netweevil/runs/experiment-baseline-matrix.gpkg
    ```

2. Run the study:

    ```bash
    cargo run -p netweevil-cli -- experiment run \
      examples/experiments/baseline_sweep.yml
    ```

3. Read the summary in `.netweevil/runs/experiment-<id>.json`.

### Study file fields

| Field | Meaning |
| --- | --- |
| `experiment.id`, `experiment.label` | Identity of the study |
| `experiment.dataset` | Dataset id for all scenarios |
| `scenarios[].id` | Optional scenario id |
| `scenarios[].profile` | Profile file |
| `scenarios[].analysis` | `route`, `od`, `matrix` or `service_area` |
| `scenarios[].request` | Request file, for `route`, `od` and `service_area` |
| `scenarios[].origins`, `scenarios[].destinations` | Point sets, for `matrix` |
| `scenarios[].out` | Optional result file. The extension selects the format |

The summary reports the status (`succeeded`, `failed` or `partial`), the
scenario count, the succeeded and failed counts, and one entry per scenario.

## Render a report

1. Find the run manifest in `.netweevil/runs/`.
2. Render it:

    ```bash
    cargo run -p netweevil-cli -- report render \
      .netweevil/runs/<run-manifest>.json \
      --out .netweevil/reports/example-report.html
    ```

| `--out` | Result |
| --- | --- |
| A `.md` path | Markdown report |
| A `.html` path | HTML report |
| Any other path | A directory with `index.md`, `index.html`, a copy of the manifest and the result file when available |
| Omitted | Markdown on standard output |

Rendered reports go to `.netweevil/reports/` by convention.

## Related pages

- [Batch analysis](batch-analysis.md).
- [Simulation](simulation.md).
- [Benchmarking](benchmarking.md).
