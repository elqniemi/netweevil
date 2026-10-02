//! `netweevil bench trip` — trip optimization quality and latency on real
//! matrices.
//!
//! For each size `n` the first `n` corpus origins become a roundtrip. The
//! stop matrix is computed once; the default solver run is then compared
//! with Held–Karp (when exact search is possible), the assignment lower
//! bound, and the best of several longer seeded runs. A final end-to-end
//! `execute_trip` call times snapping, matrix, solving and leg routing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Args;
use netweevil_persist::{WorkspacePaths, write_json};
use netweevil_query::trip_solver::{
    SolverOptions, TourMethod, TourProblem, TourSolution, assignment_lower_bound,
    default_iterations, solve,
};
use netweevil_query::{LabeledPoint, PreparedRoutingEngine, TripMode, TripRequest, TripStop};
use serde::Serialize;

use super::{CorpusRow, EngineChoice, load_engine, read_corpus, snap_options};

#[derive(Args, Debug)]
pub(crate) struct BenchTripArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    /// Stops are the first N corpus origins.
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long, value_delimiter = ',', default_values_t = [10, 50, 200, 1000])]
    sizes: Vec<usize>,
    /// Seeded runs, each with `--reference-factor` times the default
    /// iterations, whose best order is the reference for large sizes.
    #[arg(long, default_value_t = 8)]
    reference_runs: usize,
    #[arg(long, default_value_t = 5)]
    reference_factor: usize,
    /// Skip the O(n³) assignment bound above this size.
    #[arg(long, default_value_t = 1000)]
    bound_max_size: usize,
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct TripBenchRow {
    stops: usize,
    matrix_ms: f64,
    method: &'static str,
    solve_ms: f64,
    iterations: usize,
    cost: f64,
    input_order_cost: Option<f64>,
    construction_cost: Option<f64>,
    exact_cost: Option<f64>,
    heuristic_only_cost: Option<f64>,
    heuristic_only_ms: Option<f64>,
    reference_cost: Option<f64>,
    reference_ms: Option<f64>,
    assignment_bound: Option<f64>,
    end_to_end_ms: f64,
    end_to_end_matrix_ms: f64,
    end_to_end_solve_ms: f64,
    end_to_end_legs_ms: f64,
}

fn trip_request(rows: &[&CorpusRow]) -> TripRequest {
    TripRequest {
        trip_id: format!("bench-{}", rows.len()),
        stops: rows
            .iter()
            .map(|row| TripStop {
                point: LabeledPoint {
                    id: row.id.clone(),
                    lon: row.source_lon,
                    lat: row.source_lat,
                    z: None,
                },
                service_s: 0.0,
                time_window: None,
            })
            .collect(),
        mode: TripMode::Roundtrip,
        departure_s: 0.0,
        allow_late: false,
        snap: snap_options(),
        returns: Default::default(),
        optimizer: Default::default(),
        vehicle: None,
    }
}

/// A roundtrip over the first `size` corpus origins that reach each other.
/// Corpus pairs are only routable within themselves, so stops whose row or
/// column is mostly unroutable (a separate network component) are replaced
/// by later rows.
fn connected_trip(
    engine: &PreparedRoutingEngine,
    corpus: &[CorpusRow],
    size: usize,
) -> Result<(TripRequest, TourProblem, f64)> {
    let mut rejected = BTreeSet::new();
    loop {
        let rows = corpus
            .iter()
            .enumerate()
            .filter(|(index, _)| !rejected.contains(index))
            .take(size)
            .collect::<Vec<_>>();
        if rows.len() < size {
            bail!("the corpus has fewer than {size} mutually routable origins");
        }
        let request = trip_request(&rows.iter().map(|(_, row)| *row).collect::<Vec<_>>());
        let started = Instant::now();
        let problem = engine.trip_tour_problem(&request)?;
        let matrix_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let isolated = (0..size)
            .filter(|&stop| {
                let missing = (0..size)
                    .filter(|&other| {
                        other != stop
                            && !(problem.cost(stop, other).is_finite()
                                && problem.cost(other, stop).is_finite())
                    })
                    .count();
                missing * 2 > size
            })
            .map(|stop| rows[stop].0)
            .collect::<Vec<_>>();
        if isolated.is_empty() {
            return Ok((request, problem, matrix_ms));
        }
        rejected.extend(isolated);
    }
}

fn timed(problem: &TourProblem, options: &SolverOptions) -> (TourSolution, f64) {
    let started = Instant::now();
    let solution = solve(problem, options);
    (solution, started.elapsed().as_secs_f64() * 1_000.0)
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn gap(cost: f64, reference: Option<f64>) -> String {
    reference.map_or_else(
        || "-".into(),
        |reference| format!("{:+.2}%", (cost / reference - 1.0) * 100.0),
    )
}

pub(crate) fn bench_trip(paths: &WorkspacePaths, args: BenchTripArgs) -> Result<()> {
    let corpus = read_corpus(&args.corpus)?;
    let largest = args.sizes.iter().copied().max().unwrap_or(0);
    if largest > corpus.len() {
        bail!(
            "--sizes needs {largest} corpus rows but {} has {}",
            args.corpus.display(),
            corpus.len()
        );
    }
    let loaded = load_engine(
        paths,
        &args.dataset,
        &args.profile,
        EngineChoice::Accelerated,
    )?;
    let engine = &loaded.engine;
    let mut rows = Vec::new();
    for &size in &args.sizes {
        let (request, problem, matrix_ms) = connected_trip(engine, &corpus, size)?;
        let (solution, solve_ms) = timed(&problem, &SolverOptions::default());
        let exact = solution.proven_optimal.then_some(solution.value.cost);
        let heuristic_only = (solution.method != TourMethod::LocalSearch).then(|| {
            timed(
                &problem,
                &SolverOptions {
                    exact_max_stops: 0,
                    ..SolverOptions::default()
                },
            )
        });
        let reference = (solution.method == TourMethod::LocalSearch).then(|| {
            let started = Instant::now();
            let best = (1..=args.reference_runs as u64)
                .map(|seed| {
                    solve(
                        &problem,
                        &SolverOptions {
                            seed,
                            max_iterations: Some(
                                default_iterations(problem.size()) * args.reference_factor,
                            ),
                            ..SolverOptions::default()
                        },
                    )
                    .value
                    .cost
                })
                .fold(solution.value.cost, f64::min);
            (best, started.elapsed().as_secs_f64() * 1_000.0)
        });
        let assignment_bound =
            (size <= args.bound_max_size).then(|| assignment_lower_bound(&problem));
        let started = Instant::now();
        let trip = engine.execute_trip(&request)?;
        let end_to_end_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let phases = &trip.optimization;
        let row = TripBenchRow {
            stops: size,
            matrix_ms,
            method: solution.method.as_str(),
            solve_ms,
            iterations: solution.iterations,
            cost: solution.value.cost,
            input_order_cost: finite(solution.input_order_value.cost)
                .filter(|_| solution.input_order_value.missing_legs == 0),
            construction_cost: finite(solution.construction_value.cost),
            exact_cost: exact,
            heuristic_only_cost: heuristic_only.as_ref().map(|(found, _)| found.value.cost),
            heuristic_only_ms: heuristic_only.as_ref().map(|(_, ms)| *ms),
            reference_cost: reference.map(|(cost, _)| cost),
            reference_ms: reference.map(|(_, ms)| ms),
            assignment_bound,
            end_to_end_ms,
            end_to_end_matrix_ms: phases.matrix_ms,
            end_to_end_solve_ms: phases.solve_ms,
            end_to_end_legs_ms: phases.legs_ms,
        };
        print_row(&row);
        rows.push(row);
    }
    if let Some(path) = args.json.as_deref() {
        write_report(path, &rows)?;
    }
    Ok(())
}

fn print_row(row: &TripBenchRow) {
    println!(
        "n={:<5} matrix {:>8.1} ms  solve {:>8.1} ms ({}, {} kicks)",
        row.stops, row.matrix_ms, row.solve_ms, row.method, row.iterations
    );
    println!(
        "        end-to-end {:.1} ms (matrix {:.1}, solve {:.1}, legs {:.1})",
        row.end_to_end_ms,
        row.end_to_end_matrix_ms,
        row.end_to_end_solve_ms,
        row.end_to_end_legs_ms
    );
    println!(
        "        cost {:.1}  vs input order {}  vs construction {}  vs exact {}  vs best-of-runs {}  vs assignment bound {}",
        row.cost,
        gap(row.cost, row.input_order_cost),
        gap(row.cost, row.construction_cost),
        gap(row.cost, row.exact_cost),
        gap(row.cost, row.reference_cost),
        gap(row.cost, row.assignment_bound),
    );
    if let (Some(cost), Some(ms)) = (row.heuristic_only_cost, row.heuristic_only_ms) {
        println!(
            "        local search alone {:.1} ({} vs exact) in {:.1} ms",
            cost,
            gap(cost, row.exact_cost),
            ms
        );
    }
    if let Some(ms) = row.reference_ms {
        println!(
            "        reference runs took {:.1} s",
            Duration::from_secs_f64(ms / 1_000.0).as_secs_f64()
        );
    }
}

fn write_report(path: &Path, rows: &[TripBenchRow]) -> Result<()> {
    write_json(path, &rows)
}
