//! Runs every query of `fixtures/queries.json` against its fixture and checks the expectations.

use netweevil_indoor::{Input, Loc, Profile, Route, Router, compile};
use serde_json::Value;
use std::path::Path;

fn check(r: &Route, e: &Value) -> Vec<String> {
    let mut errs = vec![];
    let strs = |k: &str| -> Vec<String> {
        e.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let num = |k: &str| e.get(k).and_then(|v| v.as_f64());
    let want = strs("must_visit_spaces");
    let mut it = r.spaces.iter();
    for w in &want {
        if !it.any(|s| s == w) {
            errs.push(format!("spaces {:?} lack ordered {:?}", r.spaces, want));
            break;
        }
    }
    for s in strs("forbid_spaces") {
        if r.spaces.contains(&s) {
            errs.push(format!("visits forbidden space {s} ({:?})", r.spaces));
        }
    }
    let ids: Vec<&str> = r.segments.iter().map(|s| s.id.as_str()).collect();
    let kinds: Vec<&str> = r
        .segments
        .iter()
        .flat_map(|s| [s.kind.as_str(), s.typ])
        .collect();
    for i in strs("must_use_ids") {
        if !ids.contains(&i.as_str()) {
            errs.push(format!("does not use {i} ({ids:?})"));
        }
    }
    for i in strs("forbid_ids") {
        if ids.contains(&i.as_str()) {
            errs.push(format!("uses forbidden {i}"));
        }
    }
    for k in strs("must_use_kind") {
        if !kinds.contains(&k.as_str()) {
            errs.push(format!("no {k} segment ({kinds:?})"));
        }
    }
    for k in strs("forbid_kind") {
        if kinds.contains(&k.as_str()) {
            errs.push(format!("uses forbidden kind {k}"));
        }
    }
    for (key, v) in [
        ("length", r.length),
        ("duration", r.duration),
        ("climb", r.climb),
    ] {
        if let Some(m) = num(&format!("min_{key}"))
            && v < m
        {
            errs.push(format!("{key} {v:.2} < {m}"));
        }
        if let Some(m) = num(&format!("max_{key}"))
            && v > m
        {
            errs.push(format!("{key} {v:.2} > {m}"));
        }
    }
    errs
}

#[test]
fn fixture_queries() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let text = std::fs::read_to_string(dir.join("queries.json")).expect("fixtures/queries.json");
    let q: Value = serde_json::from_str(&text).unwrap();
    let pdir = dir.join(q["profiles_dir"].as_str().unwrap_or("profiles"));
    let mut failures = vec![];
    let mut n = 0;
    for (file, queries) in q["fixtures"].as_object().unwrap() {
        let input = Input::from_json(&std::fs::read_to_string(dir.join(file)).unwrap()).unwrap();
        let (g, report) = compile(&input);
        for w in &report.warnings {
            eprintln!("{file}: warning: {w}");
        }
        let router = Router::new(g);
        let mut ctx = router.ctx();
        for qq in queries.as_array().unwrap() {
            n += 1;
            let name = qq["name"].as_str().unwrap_or("?");
            let pname = qq["profile"].as_str().unwrap_or("walk");
            let ppath = pdir.join(format!("{pname}.json"));
            let prof = if ppath.exists() {
                Profile::resolve(ppath.to_str().unwrap()).unwrap()
            } else {
                Profile::builtin(pname).unwrap()
            };
            let cp = router.profile(&prof);
            let from: Loc = serde_json::from_value(qq["from"].clone()).unwrap();
            let to: Loc = serde_json::from_value(qq["to"].clone()).unwrap();
            let e = &qq["expect"];
            let found = e["found"].as_bool().unwrap_or(true);
            match router.route(&mut ctx, &cp, &from, &to) {
                Ok(r) => {
                    let summary = format!(
                        "len {:.2} dur {:.1} climb {:.2} spaces {:?} segs {:?}",
                        r.length,
                        r.duration,
                        r.climb,
                        r.spaces,
                        r.segments
                            .iter()
                            .map(|s| format!("{}:{}", s.kind, s.id))
                            .collect::<Vec<_>>()
                    );
                    eprintln!("{file}/{name}: {summary}");
                    if !found {
                        failures.push(format!("{file}/{name}: expected no route, got {summary}"));
                    }
                    for err in check(&r, e) {
                        failures.push(format!("{file}/{name}: {err}"));
                    }
                }
                Err(err) => {
                    eprintln!("{file}/{name}: {err}");
                    if found {
                        failures.push(format!("{file}/{name}: {err}"));
                    }
                }
            }
        }
    }
    for f in &failures {
        eprintln!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} of {n} fixture queries failed",
        failures.len()
    );
}
