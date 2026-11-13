//! Multi-stop routes (`multistop.rs`, `tsp.rs`): exact order against brute force, heuristic
//! quality, legs that join, closures in the matrix and the legs, and `POST /route` with `via`.

use netweevil_indoor::tsp::{self, Matrix};
use netweevil_indoor::{Closure, Input, Loc, Profile, Router, Trip, compile};
use serde_json::{Value, json};

/// Small deterministic random numbers (xorshift).
struct Rng(u64);

impl Rng {
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 10_000) as f32 / 10_000.0
    }
}

/// Asymmetric random costs, with some pairs unreachable when `holes`.
fn random(rng: &mut Rng, k: usize, holes: bool) -> Matrix {
    let mut c = vec![f32::INFINITY; k * k];
    for i in 0..k {
        for j in 0..k {
            if i != j && !(holes && rng.f() < 0.1) {
                c[i * k + j] = 1.0 + 100.0 * rng.f();
            }
        }
    }
    Matrix { k, c }
}

/// Points in a square: symmetric distances, as in a building.
fn euclid(rng: &mut Rng, k: usize) -> Matrix {
    let p: Vec<(f32, f32)> = (0..k).map(|_| (rng.f() * 100.0, rng.f() * 100.0)).collect();
    let mut c = vec![f32::INFINITY; k * k];
    for i in 0..k {
        for j in 0..k {
            if i != j {
                c[i * k + j] = ((p[i].0 - p[j].0).powi(2) + (p[i].1 - p[j].1).powi(2)).sqrt();
            }
        }
    }
    Matrix { k, c }
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![vec![]];
    }
    let mut out = vec![];
    for p in permutations(n - 1) {
        for i in 0..=p.len() {
            let mut q = p.clone();
            q.insert(i, n - 1);
            out.push(q);
        }
    }
    out
}

fn brute(m: &Matrix, n: usize, end: bool) -> f32 {
    permutations(n)
        .iter()
        .map(|o| tsp::path_cost(m, o, end))
        .fold(f32::INFINITY, f32::min)
}

#[test]
fn held_karp_matches_brute_force() {
    let mut rng = Rng(7);
    for n in 1..=8 {
        for end in [false, true] {
            for holes in [false, true] {
                for _ in 0..12 {
                    let m = random(&mut rng, n + 2, holes);
                    let best = brute(&m, n, end);
                    match tsp::held_karp(&m, n, end) {
                        Some(o) => {
                            let mut s = o.clone();
                            s.sort();
                            assert_eq!(s, (0..n).collect::<Vec<_>>());
                            let c = tsp::path_cost(&m, &o, end);
                            assert!(
                                (c - best).abs() <= 1e-3 * best.max(1.0),
                                "n {n}: {c} vs {best}"
                            );
                        }
                        None => assert!(!best.is_finite(), "n {n}: none, brute {best}"),
                    }
                }
            }
        }
    }
}

#[test]
fn heuristic_is_close_to_exact() {
    let mut rng = Rng(11);
    let (mut sum_h, mut sum_x) = (0.0, 0.0);
    for n in [8, 10, 12] {
        for end in [false, true] {
            for _ in 0..10 {
                let m = euclid(&mut rng, n + 2);
                let x = tsp::path_cost(&m, &tsp::held_karp(&m, n, end).unwrap(), end);
                let h = tsp::path_cost(&m, &tsp::heuristic(&m, n, end), end);
                assert!(h >= x - 1e-3, "heuristic {h} below exact {x}");
                assert!(h <= x * 1.15, "n {n}: heuristic {h}, exact {x}");
                sum_h += h;
                sum_x += x;
            }
        }
    }
    assert!(sum_h <= sum_x * 1.03, "mean gap {}", sum_h / sum_x);
}

#[test]
fn heuristic_improves_nearest_neighbour_on_many_stops() {
    let mut rng = Rng(3);
    for _ in 0..5 {
        let n = 40;
        let m = euclid(&mut rng, n + 2);
        let o = tsp::heuristic(&m, n, true);
        let mut s = o.clone();
        s.sort();
        assert_eq!(s, (0..n).collect::<Vec<_>>());
        // No single 2-opt reversal improves the result.
        let c = tsp::path_cost(&m, &o, true);
        for i in 0..n {
            for j in i + 1..n {
                let mut q = o.clone();
                q[i..=j].reverse();
                assert!(tsp::path_cost(&m, &q, true) >= c - 1e-3 * c);
            }
        }
    }
}

#[test]
#[ignore = "timing: cargo test --release --test multistop -- --ignored --nocapture"]
fn held_karp_timing() {
    let mut rng = Rng(5);
    for n in 8..=16 {
        let m = euclid(&mut rng, n + 2);
        let t = std::time::Instant::now();
        tsp::held_karp(&m, n, true).unwrap();
        let x = t.elapsed();
        let t = std::time::Instant::now();
        tsp::heuristic(&m, n, true);
        println!("n {n}: exact {x:?}, heuristic {:?}", t.elapsed());
    }
    for n in [25, 50] {
        let m = euclid(&mut rng, n + 2);
        let t = std::time::Instant::now();
        tsp::heuristic(&m, n, true);
        println!("n {n}: heuristic {:?}", t.elapsed());
    }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]])
}

/// L0: corridor c (0..60 x 0..3) with rooms R0..R4 above it (12 m apart), each with a door
/// at its west end and R1 also at its east end. L1: corridor c1, a lift at x 1, stairs at x 59.
/// Toilets t0 (in R0) and t4 (in R4).
fn building() -> Router {
    let mut surfaces = vec![
        json!({"id": "c", "space": "c", "level": "L0", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 60.0, 3.0)}),
        json!({"id": "c1", "space": "c1", "level": "L1", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 60.0, 3.0)}),
    ];
    let mut portals = vec![
        json!({"id": "d1e", "kind": "door", "level": "L0", "segment": [[21.0, 3.0], [22.0, 3.0]]}),
    ];
    for k in 0..5 {
        let x0 = 2.0 + 12.0 * k as f64;
        surfaces.push(json!({"id": format!("R{k}"), "space": format!("R{k}"), "level": "L0", "class": "room", "polygon": rect(x0, 3.0, x0 + 10.0, 10.0)}));
        portals.push(json!({"id": format!("d{k}"), "kind": "door", "level": "L0", "segment": [[x0 + 1.0, 3.0], [x0 + 2.0, 3.0]]}));
    }
    let v = json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 3.5}],
        "surfaces": surfaces,
        "portals": portals,
        "verticals": [
            {"id": "lift", "kind": "lift", "wait": 5.0, "stops": [{"level": "L0", "at": [1.0, 1.5]}, {"level": "L1", "at": [1.0, 1.5]}]},
            {"id": "st", "kind": "stairs", "stops": [{"level": "L0", "at": [59.0, 1.5]}, {"level": "L1", "at": [59.0, 1.5]}]}
        ],
        "pois": [
            {"id": "t0", "category": "toilet", "at": [5.0, 8.0], "level": "L0"},
            {"id": "t4", "category": "toilet", "at": [53.0, 8.0], "level": "L0"}
        ]
    });
    let input: Input = serde_json::from_value(v).unwrap();
    Router::new(compile(&input).0)
}

fn pt(x: f64, y: f64, level: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        ..Default::default()
    }
}

fn space(id: &str) -> Loc {
    Loc {
        space: Some(id.into()),
        ..Default::default()
    }
}

fn trip(
    r: &Router,
    from: &Loc,
    via: &[Loc],
    to: Option<&Loc>,
    opt: bool,
    list: &[Closure],
) -> Trip {
    let cp = r.profile(&Profile::builtin("walk").unwrap());
    let mut ctx = r.ctx();
    let ov = (!list.is_empty()).then(|| r.overlay(list));
    r.route_stops(&mut ctx, &cp, from, via, to, opt, ov.as_deref())
        .unwrap()
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6 && (a[2] - b[2]).abs() < 1e-6
}

#[test]
fn legs_join_and_add_up() {
    let r = building();
    let via = [space("R3"), pt(30.0, 1.5, "L1"), space("R0")];
    let t = trip(
        &r,
        &pt(0.5, 1.5, "L0"),
        &via,
        Some(&pt(58.0, 1.5, "L0")),
        false,
        &[],
    );
    assert_eq!(t.order, vec![0, 1, 2]);
    assert!(t.optimised.is_none());
    assert_eq!(t.legs.len(), 4);
    for w in t.legs.windows(2) {
        let (a, b) = (w[0].coords.last().unwrap(), w[1].coords.first().unwrap());
        assert!(close(*a, *b), "{a:?} vs {b:?}");
    }
    let sum = |f: fn(&netweevil_indoor::Route) -> f64| t.legs.iter().map(f).sum::<f64>();
    assert!((t.duration - sum(|l| l.duration)).abs() < 1e-9);
    assert!((t.length - sum(|l| l.length)).abs() < 1e-9);
    assert!((t.cost - sum(|l| l.cost)).abs() < 1e-9);
    // Each leg is the plain route between its stops.
    let cp = r.profile(&Profile::builtin("walk").unwrap());
    let one = r.route(&mut r.ctx(), &cp, &via[0], &via[1]).unwrap();
    assert!((one.cost - t.legs[1].cost).abs() < 1e-6);
    assert!(
        t.legs[1]
            .segments
            .iter()
            .any(|s| s.kind == "lift" || s.kind == "stairs")
    );
}

#[test]
fn optimised_order_is_the_cheapest() {
    let r = building();
    let from = pt(0.5, 1.5, "L0");
    let to = pt(58.0, 1.5, "L0");
    let via = [
        space("R4"),
        space("R1"),
        pt(30.0, 1.5, "L1"),
        space("R2"),
        space("R0"),
    ];
    for to in [Some(&to), None] {
        let best = permutations(via.len())
            .into_iter()
            .map(|o| {
                let v: Vec<Loc> = o.iter().map(|&i| via[i].clone()).collect();
                trip(&r, &from, &v, to, false, &[]).cost
            })
            .fold(f64::INFINITY, f64::min);
        let t = trip(&r, &from, &via, to, true, &[]);
        assert_eq!(t.optimised, Some("exact"));
        assert!(
            t.cost <= best * 1.001 + 1e-6,
            "optimised {} vs best {best}",
            t.cost
        );
        let mut o = t.order.clone();
        o.sort();
        assert_eq!(o, vec![0, 1, 2, 3, 4]);
    }
}

#[test]
fn closures_hold_in_matrix_and_legs() {
    let r = building();
    let c: Closure = serde_json::from_value(json!({"id": "x", "elements": ["d1"]})).unwrap();
    let from = pt(0.5, 1.5, "L0");
    let via = [space("R4"), space("R1"), space("R0")];
    let free = trip(&r, &from, &via, None, true, &[]);
    let t = trip(&r, &from, &via, None, true, std::slice::from_ref(&c));
    for leg in &t.legs {
        assert!(leg.segments.iter().all(|s| s.id != "d1"));
    }
    assert!(
        free.legs
            .iter()
            .any(|l| l.segments.iter().any(|s| s.id == "d1"))
    );
    assert!(t.closures.iter().any(|h| h.id.as_deref() == Some("x")));
    // Brute force with the closure gives the same cost.
    let best = permutations(3)
        .into_iter()
        .map(|o| {
            let v: Vec<Loc> = o.iter().map(|&i| via[i].clone()).collect();
            trip(&r, &from, &v, None, false, std::slice::from_ref(&c)).cost
        })
        .fold(f64::INFINITY, f64::min);
    assert!(t.cost <= best * 1.001 + 1e-6);
}

#[test]
fn category_stop_continues_from_the_chosen_poi() {
    let r = building();
    let toilet = Loc {
        category: Some("toilet".into()),
        ..Default::default()
    };
    let t = trip(
        &r,
        &pt(50.0, 1.5, "L0"),
        &[toilet],
        Some(&space("R3")),
        false,
        &[],
    );
    let hit = t.legs[0].to_poi.as_ref().unwrap();
    assert_eq!(hit.id, "t4");
    assert_eq!(
        t.legs[1].from_poi.as_ref().map(|p| p.id.as_str()),
        Some("t4")
    );
    assert!(close(
        *t.legs[0].coords.last().unwrap(),
        t.legs[1].coords[0]
    ));
}
