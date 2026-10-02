use super::*;

fn row(id: &str) -> CorpusRow {
    CorpusRow {
        id: id.to_string(),
        source_lon: 4.9,
        source_lat: 52.3,
        target_lon: 5.1,
        target_lat: 52.4,
        bucket: Bucket::Medium,
    }
}

fn record(index: usize, id: &str, latency_ms: f64, duration_s: Option<f64>) -> RequestRecord {
    RequestRecord {
        index,
        id: id.to_string(),
        bucket: Some("short".to_string()),
        ok: duration_s.is_some(),
        latency_ms,
        duration_s,
        distance_m: duration_s.map(|_| 1000.0),
        generalized_cost: duration_s,
        matrix_cell_counts: None,
        error: duration_s.is_none().then(|| "unreachable".to_string()),
    }
}

#[test]
fn paced_requests_wait_between_measurements() {
    let interval = Duration::from_millis(10);
    let (samples, wall) = drive(&[0, 1, 2], 1, 1, interval, |id| {
        (id.to_string(), None, Outcome::ok_without_quality())
    });
    assert_eq!(samples.len(), 3);
    assert!(wall >= interval * 2);
}

#[test]
fn classifies_distance_buckets() {
    for (distance_m, bucket) in [
        (900.0, Bucket::Short),
        (4_999.0, Bucket::Short),
        (5_000.0, Bucket::Medium),
        (30_000.0, Bucket::Medium),
        (30_001.0, Bucket::Long),
    ] {
        assert_eq!(Bucket::classify(distance_m), bucket, "{distance_m}");
    }
}

#[test]
fn interpolates_percentiles() {
    let sorted = [1.0, 2.0, 3.0, 4.0];
    for (p, expected) in [(0.0, 1.0), (0.5, 2.5), (1.0, 4.0)] {
        assert!((percentile(&sorted, p) - expected).abs() < 1e-9, "p={p}");
    }
    assert_eq!(percentile(&[], 0.5), 0.0);
}

#[test]
fn offsets_points_by_distance_and_bearing() {
    let origin = (6.5665, 53.2194);
    let north = offset_point(origin, 0.0, 10_000.0);
    assert!(north.1 > origin.1);
    assert!((haversine_m(origin, north) - 10_000.0).abs() < 5.0);
    let east = offset_point(origin, std::f64::consts::FRAC_PI_2, 25_000.0);
    assert!(east.0 > origin.0);
    assert!((haversine_m(origin, east) - 25_000.0).abs() < 5.0);
}

#[test]
fn samples_reproducibly_from_a_seed() {
    let sample = |seed| {
        let mut rng = SplitMix64::new(seed);
        (0..8).map(|_| rng.next_unit()).collect::<Vec<f64>>()
    };
    let a = sample(42);
    assert_eq!(a, sample(42));
    assert!(a.iter().all(|value| (0.0..1.0).contains(value)));
}

#[test]
fn reads_canonical_corpus_and_rejects_invalid_rows() {
    let dir = std::env::temp_dir().join(format!("netweevil-bench-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let current = dir.join("current.csv");
    std::fs::write(
        &current,
        "id,source_lon,source_lat,target_lon,target_lat,bucket\nb,6.5,53.2,6.9,53.6,long\n",
    )
    .unwrap();
    let rows = read_corpus(&current).unwrap();
    assert_eq!(rows[0].bucket, Bucket::Long);

    let invalid = dir.join("invalid.csv");
    for content in [
        "id,source_lon,source_lat,target_lon,target_lat\na,NaN,53.2,6.9,53.6\n",
        "id,source_lon,source_lat,target_lon,target_lat\na,6.5,91,6.9,53.6\n",
        "id,source_lon,source_lat,target_lon,target_lat\na,6.5,53.2,6.9,53.6\na,6.5,53.2,6.9,53.6\n",
        "id,source_lon,source_lat,target_lon,target_lat,bucket\na,6.5,53.2,6.9,53.6,invalid\n",
    ] {
        std::fs::write(&invalid, content).unwrap();
        assert!(read_corpus(&invalid).is_err(), "{content}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn builds_engine_requests() {
    assert_eq!(
        osrm_route_url("http://127.0.0.1:5000/", "driving", &row("r1")),
        "http://127.0.0.1:5000/route/v1/driving/4.9,52.3;5.1,52.4?overview=false&radiuses=500;500"
    );

    assert_eq!(
        netweevil_route_url("http://127.0.0.1:8080/"),
        "http://127.0.0.1:8080/v1/route"
    );
    let body = netweevil_route_body(&row("r1"), Some("car_v1"));
    assert_eq!(body["profile_id"], "car_v1");
    assert_eq!(body["request"]["route_id"], "r1");
    assert_eq!(body["request"]["returns"]["geometry"], "none");
    assert_eq!(body["request"]["origin"]["lon"], 4.9);
    assert!(
        netweevil_route_body(&row("r1"), None)
            .get("profile_id")
            .is_none()
    );

    // Valhalla must answer in OSRM units so `parse_osrm_route` can read it.
    let body = valhalla_route_body(&row("v"), "bicycle");
    assert_eq!(body["costing"], "bicycle");
    assert_eq!(body["locations"][0]["lon"], 4.9);
    assert_eq!(body["locations"][1]["search_cutoff"], 500.0);
    assert_eq!(body["format"], "osrm");
    assert_eq!(body["shape_format"], "no_shape");
    assert_eq!(body["directions_type"], "none");
}

#[test]
fn comparison_rejects_changed_coordinates_or_workloads() {
    let original = row("same-id");
    let mut changed = original.clone();
    changed.target_lon += 0.00000001;
    let baseline = BenchSettings {
        corpus_sha256: corpus_fingerprint(&[original]),
        workload: "route".to_string(),
        ..BenchSettings::default()
    };
    let mut candidate = baseline.clone();
    candidate.corpus_path = "a-copy-at-another-path.csv".to_string();
    assert!(validate_comparison(&baseline, &candidate).is_ok());
    candidate.corpus_sha256 = corpus_fingerprint(&[changed]);
    assert!(validate_comparison(&baseline, &candidate).is_err());
    candidate = baseline.clone();
    candidate.workload = "route-geometry".to_string();
    assert!(validate_comparison(&baseline, &candidate).is_err());
}

#[test]
fn parses_osrm_route_responses() {
    let outcome = parse_osrm_route(&serde_json::json!({
        "code": "Ok",
        "routes": [{ "duration": 1234.5, "distance": 20345.0 }],
    }));
    assert!(outcome.ok);
    assert_eq!(outcome.duration_s, Some(1234.5));
    assert_eq!(outcome.distance_m, Some(20345.0));

    let no_route = parse_osrm_route(&serde_json::json!({ "code": "NoRoute" }));
    assert!(!no_route.ok);
    assert!(!no_route.status_failure);
    assert!(no_route.error.unwrap().contains("NoRoute"));

    for failure in [
        serde_json::json!({ "code": "Ok", "routes": [] }),
        serde_json::json!({ "routes": [] }),
    ] {
        assert!(!parse_osrm_route(&failure).ok, "{failure}");
    }
}

#[test]
fn parses_netweevil_route_responses() {
    let outcome = parse_netweevil_route(&serde_json::json!({
        "service": {},
        "result": {
            "outcome": "legal",
            "summary": { "total_travel_time_s": 900.5,
                "total_generalized_cost": 912.25, "total_distance_m": 15000 },
        },
    }));
    assert!(outcome.ok);
    assert_eq!(outcome.duration_s, Some(900.5));
    assert_eq!(outcome.generalized_cost, Some(912.25));
    assert_eq!(outcome.distance_m, Some(15000.0));

    let unreachable = parse_netweevil_route(&serde_json::json!({
        "result": { "outcome": "unreachable", "summary": {} },
    }));
    assert!(!unreachable.ok);
    assert!(!unreachable.status_failure);
}

#[test]
fn merges_repeated_iterations_by_id() {
    let merged = merge_by_id(&[record(0, "a", 30.0, None), record(1, "a", 10.0, Some(60.0))]);
    let entry = &merged["a"];
    assert!(entry.ok);
    assert_eq!(entry.duration_s, Some(60.0));
    assert!((entry.latency_ms - 20.0).abs() < 1e-9);
}

#[test]
fn computes_relative_difference() {
    assert_eq!(relative_difference(100.0, 110.0), Some(0.1));
    assert_eq!(relative_difference(100.0, 90.0), Some(0.1));
    assert_eq!(relative_difference(0.0, 5.0), None);
}

#[test]
fn rejects_malformed_bbox() {
    assert!(parse_bbox("1,2,3").is_err());
    assert!(parse_bbox("5,52,4,53").is_err());
    let bounds = parse_bbox("4.0,52.0,5.0,53.0").unwrap();
    assert!(bounds_contain(&bounds, (4.5, 52.5)));
    assert!(!bounds_contain(&bounds, (6.0, 52.5)));
}
