use super::*;

/// Points on a 10 km square; arcs cost their length times a per-direction
/// factor in `[1, 1 + skew)`, like a road network with one-way detours.
fn road_like(size: usize, skew: f64, seed: u64) -> TourProblem {
    let mut random = Random(seed);
    let points = (0..size)
        .map(|_| (unit(&mut random) * 10_000.0, unit(&mut random) * 10_000.0))
        .collect::<Vec<_>>();
    let mut cost = vec![0.0; size * size];
    for from in 0..size {
        for to in 0..size {
            if from != to {
                let (dx, dy) = (points[from].0 - points[to].0, points[from].1 - points[to].1);
                cost[from * size + to] =
                    (dx * dx + dy * dy).sqrt() * (1.0 + skew * unit(&mut random));
            }
        }
    }
    TourProblem::new(size, cost)
}

/// Independent uniform arc costs: the hardest kind of asymmetric instance.
fn uniform(size: usize, seed: u64) -> TourProblem {
    let mut random = Random(seed);
    let cost = (0..size * size)
        .map(|index| {
            if index / size == index % size {
                0.0
            } else {
                1.0 + (random.next() % 1_000) as f64
            }
        })
        .collect();
    TourProblem::new(size, cost)
}

fn unit(random: &mut Random) -> f64 {
    (random.next() >> 11) as f64 / (1_u64 << 53) as f64
}

/// Travel time equal to cost, `service` per stop, and windows of
/// `width` seconds placed around the schedule of a hidden random order,
/// so at least that order is on time.
fn with_feasible_windows(problem: TourProblem, service: f64, width: f64, seed: u64) -> TourProblem {
    let size = problem.size();
    let mut random = Random(seed);
    let mut hidden = (1..size).collect::<Vec<_>>();
    for index in (1..hidden.len()).rev() {
        hidden.swap(index, random.between(0, index));
    }
    let mut window = vec![TimeWindow::OPEN; size];
    window[0] = TimeWindow {
        earliest_s: 0.0,
        latest_s: f64::INFINITY,
    };
    let mut clock = 0.0;
    let mut previous = 0;
    for &node in &hidden {
        clock += problem.cost(previous, node);
        let offset = unit(&mut random) * width;
        window[node] = TimeWindow {
            earliest_s: (clock - offset).max(0.0),
            latest_s: clock - offset + width,
        };
        clock += service;
        previous = node;
    }
    let time = problem.cost.clone();
    let mut services = vec![service; size];
    services[0] = 0.0;
    problem.with_schedule(time, services, window, TimeWindow::OPEN)
}

/// Every order, best first by the solver's objective.
fn brute_force(problem: &TourProblem) -> (Vec<usize>, TourValue) {
    fn permute(
        problem: &TourProblem,
        order: &mut Vec<usize>,
        rest: &mut Vec<usize>,
        best: &mut Option<(Vec<usize>, TourValue)>,
    ) {
        if rest.is_empty() {
            let value = problem.evaluate(order);
            if best
                .as_ref()
                .is_none_or(|(_, known)| value.is_better_than(known))
            {
                *best = Some((order.clone(), value));
            }
            return;
        }
        for index in 0..rest.len() {
            let node = rest.remove(index);
            order.push(node);
            permute(problem, order, rest, best);
            order.pop();
            rest.insert(index, node);
        }
    }
    let mut best = None;
    permute(
        problem,
        &mut vec![0],
        &mut (1..problem.size()).collect(),
        &mut best,
    );
    best.expect("at least one order")
}

fn heuristic(seed: u64) -> SolverOptions {
    SolverOptions {
        seed,
        exact_max_stops: 0,
        max_iterations: Some(300),
        time_budget: None,
    }
}

#[test]
fn held_karp_is_optimal_on_random_asymmetric_instances() {
    for size in 1..=8 {
        for seed in 0..10 {
            for problem in [road_like(size, 0.6, seed), uniform(size, seed)] {
                let solution = solve(&problem, &SolverOptions::default());
                let (_, optimum) = brute_force(&problem);
                assert_eq!(solution.method, TourMethod::HeldKarp);
                assert!(solution.proven_optimal);
                assert!(is_tour(&solution.order, size));
                assert!(
                    (solution.value.cost - optimum.cost).abs() <= 1e-6,
                    "size {size} seed {seed}: {} vs optimum {}",
                    solution.value.cost,
                    optimum.cost
                );
            }
        }
    }
}

#[test]
fn held_karp_with_time_windows_matches_brute_force() {
    let mut infeasible = 0;
    for size in 2..=7 {
        for seed in 0..12 {
            // Narrow windows around a hidden order, then shift one window
            // earlier on some seeds so a few instances become infeasible.
            let mut problem =
                with_feasible_windows(road_like(size, 0.4, seed), 60.0, 900.0, seed + 100);
            if seed % 4 == 3 {
                let schedule = problem.schedule.as_mut().unwrap();
                schedule.window[size - 1].latest_s = 1.0;
                schedule.window[size - 1].earliest_s = 0.0;
            }
            let solution = solve(&problem, &SolverOptions::default());
            let (_, best) = brute_force(&problem);
            assert!(is_tour(&solution.order, size));
            if best.lateness_s > 0.0 {
                infeasible += 1;
                assert!(solution.proven_infeasible, "size {size} seed {seed}");
                assert!(solution.value.lateness_s > 0.0);
                // The fallback still minimizes lateness first.
                assert!(solution.value.lateness_s <= best.lateness_s * 1.000_001 + 1e-6);
            } else {
                assert_eq!(solution.method, TourMethod::HeldKarpTimeWindows);
                assert!(solution.proven_optimal);
                assert_eq!(solution.value.lateness_s, 0.0, "size {size} seed {seed}");
                assert!((solution.value.cost - best.cost).abs() <= 1e-6);
            }
        }
    }
    assert!(infeasible > 0, "the fixture must include infeasible cases");
}

#[test]
fn local_search_is_never_worse_than_the_input_order() {
    for size in [4, 5, 7, 12, 25, 60] {
        for seed in 0..4 {
            let mut instances = vec![road_like(size, 0.5, seed), uniform(size, seed)];
            instances.push(with_feasible_windows(
                road_like(size, 0.3, seed),
                120.0,
                600.0,
                seed,
            ));
            let mut sparse = uniform(size, seed + 7);
            for index in (0..size * size).step_by(3) {
                if index / size != index % size {
                    sparse.cost[index] = f64::INFINITY;
                }
            }
            instances.push(sparse);
            for problem in instances {
                let solution = solve(&problem, &heuristic(seed));
                assert_eq!(solution.method, TourMethod::LocalSearch);
                assert!(is_tour(&solution.order, size));
                let input = problem.evaluate(&(0..size).collect::<Vec<_>>());
                assert_eq!(solution.input_order_value, input);
                assert!(
                    !input.is_better_than(&solution.value),
                    "size {size} seed {seed}: input {input:?} beat {:?}",
                    solution.value
                );
                assert!(!solution.construction_value.is_better_than(&solution.value));
            }
        }
    }
}

#[test]
fn local_search_finds_the_optimum_on_small_instances() {
    let (mut instances, mut optimal, mut worst_gap) = (0, 0, 0.0_f64);
    for size in [8, 10, 12] {
        for seed in 0..8 {
            for problem in [road_like(size, 0.5, seed), uniform(size, seed)] {
                let exact = solve(&problem, &SolverOptions::default());
                let found = solve(&problem, &heuristic(seed));
                let gap = found.value.cost / exact.value.cost - 1.0;
                assert!(gap >= -1e-9, "heuristic beat Held–Karp");
                instances += 1;
                optimal += usize::from(gap <= 1e-9);
                worst_gap = worst_gap.max(gap);
            }
        }
    }
    assert!(
        optimal * 100 >= instances * 95 && worst_gap < 0.02,
        "{optimal}/{instances} optimal, worst gap {:.3}%",
        worst_gap * 100.0
    );
}

#[test]
fn local_search_is_deterministic_for_a_seed() {
    let problem = road_like(100, 0.5, 3);
    let first = solve(&problem, &heuristic(11));
    let second = solve(&problem, &heuristic(11));
    assert_eq!(first.order, second.order);
    assert_eq!(first.iterations, 300);
}

#[test]
fn local_search_meets_feasible_time_windows() {
    for seed in 0..5 {
        let problem = with_feasible_windows(road_like(50, 0.3, seed), 300.0, 1_800.0, seed);
        let solution = solve(&problem, &heuristic(seed));
        assert_eq!(
            solution.value.lateness_s, 0.0,
            "seed {seed}: {:?}",
            solution.value
        );
        assert_eq!(solution.value.missing_legs, 0);
    }
}

#[test]
fn local_search_avoids_missing_routes_when_a_routable_order_exists() {
    let size = 40;
    let mut problem = uniform(size, 5);
    let mut random = Random(9);
    let mut hidden = (1..size).collect::<Vec<_>>();
    for index in (1..hidden.len()).rev() {
        hidden.swap(index, random.between(0, index));
    }
    let mut cycle = vec![0];
    cycle.extend(&hidden);
    let keep = cycle
        .iter()
        .zip(cycle.iter().cycle().skip(1))
        .map(|(&from, &to)| from * size + to)
        .collect::<std::collections::HashSet<_>>();
    for index in 0..size * size {
        if index / size != index % size && !keep.contains(&index) && random.next() % 10 < 8 {
            problem.cost[index] = f64::INFINITY;
        }
    }
    let solution = solve(&problem, &heuristic(1));
    assert_eq!(solution.value.missing_legs, 0);
}

#[test]
fn assignment_bound_is_below_every_tour() {
    for size in 2..=8 {
        for seed in 0..8 {
            let problem = uniform(size, seed);
            let (_, optimum) = brute_force(&problem);
            assert!(assignment_lower_bound(&problem) <= optimum.cost + 1e-6);
        }
    }
    // On costs that make the identity-shifted cycle the unique assignment,
    // the bound is tight.
    let size = 6;
    let cost = (0..size * size)
        .map(|index| {
            let (from, to) = (index / size, index % size);
            if to == (from + 1) % size { 1.0 } else { 50.0 }
        })
        .collect();
    assert_eq!(assignment_lower_bound(&TourProblem::new(size, cost)), 6.0);
}

/// Every neighborhood's O(1) cost and incremental lateness must equal a
/// full re-evaluation of the moved tour.
#[test]
fn candidate_pricing_matches_full_evaluation() {
    let size = 30;
    let problem = with_feasible_windows(road_like(size, 0.6, 4), 200.0, 400.0, 4);
    let instance = Instance::new(&problem, true);
    let mut random = Random(17);
    let mut order = (0..size).collect::<Vec<_>>();
    order[1..].reverse();
    let tour = Tour::new(&instance, &order);
    let last = tour.last();
    let mut checked = 0;
    for _ in 0..1_000 {
        let i = random.between(0, last - 3);
        let j = random.between(i + 1, last - 2);
        let k = random.between(j + 1, last - 1);
        let p = random.between(0, last - 1);
        let mut candidates = vec![
            Candidate::new(&[(0, i), (j, i + 1), (j + 1, last)]),
            Candidate::new(&[(0, i), (j + 1, k), (i + 1, j), (k + 1, last)]),
        ];
        let (start, end) = (i + 1, (i + 3).min(j));
        for moved in [(start, end), (end, start)] {
            if p + 1 < start {
                candidates.push(Candidate::new(&[
                    (0, p),
                    moved,
                    (p + 1, start - 1),
                    (end + 1, last),
                ]));
            } else if p > end {
                candidates.push(Candidate::new(&[
                    (0, start - 1),
                    (end + 1, p),
                    moved,
                    (p + 1, last),
                ]));
            }
        }
        for candidate in candidates {
            let mut moved = tour.clone();
            moved.apply(&instance, &candidate, &mut Vec::new());
            assert!(is_tour(&moved.nodes[..size], size));
            let full = problem.evaluate(&moved.nodes[..size]);
            assert!((tour.candidate_cost(&instance, &candidate) - full.cost).abs() < 1e-6);
            let lateness = tour
                .candidate_lateness(&instance, &candidate, f64::INFINITY)
                .unwrap();
            assert!(
                (lateness - full.lateness_s).abs() < 1e-6,
                "{lateness} vs {}",
                full.lateness_s
            );
            checked += 1;
        }
    }
    assert!(checked > 2_000);
}

#[test]
fn time_budget_ends_the_search() {
    let problem = road_like(400, 0.5, 2);
    let solution = solve(
        &problem,
        &SolverOptions {
            max_iterations: Some(usize::MAX),
            time_budget: Some(Duration::from_millis(50)),
            ..heuristic(0)
        },
    );
    assert!(is_tour(&solution.order, 400));
    assert!(solution.iterations < usize::MAX);
}
