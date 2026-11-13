use super::*;

const LON: f64 = 6.5665;
const LAT: f64 = 53.2194;

fn res(resolution: u8) -> Resolution {
    parse_resolution(resolution).unwrap()
}

#[test]
fn parses_and_formats_cells() {
    let cell = cell_at(LON, LAT, res(8)).unwrap();
    assert_eq!(parse_cell(&cell.to_string()).unwrap(), cell);
    assert_eq!(parse_cell(&format!(" {cell} ")).unwrap(), cell);
    assert!(parse_cell("not-a-cell").is_err());
    assert!(parse_resolution(16).is_err());
    assert_eq!(parse_resolutions(&[9, 7, 9]).unwrap(), [res(7), res(9)]);
    assert!(cell_at(f64::NAN, LAT, res(8)).is_none());
}

#[test]
fn ring_is_closed_and_contains_the_center() {
    let cell = cell_at(LON, LAT, res(9)).unwrap();
    let ring = cell_ring(cell);
    assert_eq!(ring.len(), 7);
    assert_eq!(ring.first(), ring.last());
    let center = cell_center(cell);
    assert_eq!(cell_at(center[0], center[1], res(9)), Some(cell));
}

#[test]
fn lookup_prefers_the_finest_covering_cell() {
    let coarse = cell_at(LON, LAT, res(6)).unwrap();
    let fine = cell_at(LON, LAT, res(9)).unwrap();
    let mut map = CellMap::new();
    map.insert(coarse, "coarse");
    assert_eq!(map.lookup(LON, LAT), Some((coarse, &"coarse")));
    map.insert(fine, "fine");
    assert_eq!(map.resolutions(), [res(9), res(6)]);
    assert_eq!(map.lookup(LON, LAT), Some((fine, &"fine")));
    assert_eq!(
        map.covering(LON, LAT)
            .map(|(_, value)| *value)
            .collect::<Vec<_>>(),
        ["fine", "coarse"]
    );
    // Elsewhere in the coarse cell only the coarse value applies.
    let other = coarse
        .children(res(9))
        .find(|child| *child != fine)
        .unwrap();
    let center = cell_center(other);
    assert_eq!(map.lookup(center[0], center[1]), Some((coarse, &"coarse")));
    assert_eq!(map.lookup(0.0, 0.0), None);
}

#[test]
fn aggregates_points_on_several_levels() {
    let points = [
        (LON, LAT, 2.0),
        (LON + 1e-5, LAT, 3.0),
        (LON + 0.05, LAT, 5.0),
        (f64::NAN, LAT, 1.0),
    ];
    let (stats, skipped) = aggregate_points(points, &[res(5), res(9)]);
    assert_eq!(skipped, 1);
    for resolution in [5, 9] {
        let level = stats
            .iter()
            .filter(|stat| stat.resolution == resolution)
            .collect::<Vec<_>>();
        assert_eq!(level.iter().map(|stat| stat.count).sum::<u64>(), 3);
        assert_eq!(level.iter().map(|stat| stat.weight).sum::<f64>(), 10.0);
    }
    let fine = stats
        .iter()
        .find(|stat| stat.cell == cell_at(LON, LAT, res(9)).unwrap())
        .unwrap();
    assert_eq!((fine.count, fine.weight), (2, 5.0));
    assert!(
        stats.windows(2).all(|pair| {
            (pair[0].resolution, pair[0].cell) < (pair[1].resolution, pair[1].cell)
        })
    );
}

#[test]
fn resampling_preserves_the_total_weight() {
    let cell = cell_at(LON, LAT, res(7)).unwrap();
    let rows = [WeightedCell { cell, weight: 49.0 }];
    let finer = resample_cells(&rows, res(8)).unwrap();
    assert_eq!(finer.len(), 7);
    assert!(finer.iter().all(|row| (row.weight - 7.0).abs() < 1e-12));
    let coarser = resample_cells(&finer, res(6)).unwrap();
    assert_eq!(coarser.len(), 1);
    assert_eq!(coarser[0].cell, cell.parent(res(6)).unwrap());
    assert!((coarser[0].weight - 49.0).abs() < 1e-9);
    assert!(resample_cells(&rows, res(15)).is_err());
}

#[test]
fn covers_a_bbox_with_cell_centers() {
    let bbox = [6.50, 53.18, 6.62, 53.25];
    let cells = cover_bbox(bbox, res(8), 100_000).unwrap();
    // About 8 km by 7.8 km over 0.737 km2 cells.
    assert!((60..110).contains(&cells.len()), "{} cells", cells.len());
    for cell in &cells {
        let center = cell_center(*cell);
        assert!(center[0] >= bbox[0] && center[0] <= bbox[2]);
        assert!(center[1] >= bbox[1] && center[1] <= bbox[3]);
    }
    // Every cell of a dense lattice with its centre inside is found.
    for step_lon in 0..60 {
        for step_lat in 0..60 {
            let lon = bbox[0] + (bbox[2] - bbox[0]) * f64::from(step_lon) / 59.0;
            let lat = bbox[1] + (bbox[3] - bbox[1]) * f64::from(step_lat) / 59.0;
            let cell = cell_at(lon, lat, res(8)).unwrap();
            let center = cell_center(cell);
            let inside = center[0] >= bbox[0]
                && center[0] <= bbox[2]
                && center[1] >= bbox[1]
                && center[1] <= bbox[3];
            assert_eq!(cells.binary_search(&cell).is_ok(), inside);
        }
    }
    assert!(cover_bbox(bbox, res(12), 1000).is_err());
    let tiny = cover_bbox([LON, LAT, LON, LAT], res(8), 10).unwrap();
    assert_eq!(tiny, [cell_at(LON, LAT, res(8)).unwrap()]);
}

#[test]
fn covers_a_polygon_and_compacts() {
    let triangle = [[6.50, 53.18], [6.62, 53.18], [6.50, 53.25]];
    let bbox_cells = cover_bbox([6.50, 53.18, 6.62, 53.25], res(8), 100_000).unwrap();
    let cells = cover_polygon(&triangle, res(8), 100_000).unwrap();
    assert!(!cells.is_empty() && cells.len() < bbox_cells.len() * 2 / 3);

    let parent = cell_at(LON, LAT, res(7)).unwrap();
    let children = parent.children(res(9)).collect::<Vec<_>>();
    let compacted = compact_cells(children.clone()).unwrap();
    assert_eq!(compacted, [parent]);
    let mut expanded = uncompact_cells(&compacted, res(9)).unwrap();
    expanded.sort_unstable();
    let mut expected = children;
    expected.sort_unstable();
    assert_eq!(expanded, expected);
    assert!(uncompact_cells(&expected, res(7)).is_err());
}

#[test]
fn reads_cell_csv_and_writes_geojson() {
    let a = cell_at(LON, LAT, res(8)).unwrap();
    let b = cell_at(LON + 0.1, LAT, res(8)).unwrap();
    let text = format!("cell,population\n{a},10\n{b},4\n{a},2.5\n");
    let rows = read_cells_csv(&text, Some("population")).unwrap();
    assert_eq!(
        rows,
        [
            WeightedCell {
                cell: a,
                weight: 12.5
            },
            WeightedCell {
                cell: b,
                weight: 4.0
            }
        ]
    );
    assert_eq!(read_cells_csv(&text, None).unwrap()[0].weight, 2.0);
    assert!(read_cells_csv(&text, Some("missing")).is_err());
    assert!(read_cells_csv("cell,weight\nxyz,1\n", None).is_err());

    let geojson = cells_geojson(rows.iter().map(|row| {
        let mut properties = serde_json::Map::new();
        properties.insert("weight".into(), row.weight.into());
        (row.cell, properties)
    }));
    let feature = &geojson["features"][0];
    assert_eq!(feature["properties"]["cell"], a.to_string());
    assert_eq!(feature["properties"]["resolution"], 8);
    assert_eq!(feature["properties"]["weight"], 12.5);
    assert_eq!(
        feature["geometry"]["coordinates"][0]
            .as_array()
            .unwrap()
            .len(),
        7
    );

    let parsed: WeightedCell = serde_json::from_str(&format!("{{\"cell\":\"{a}\"}}")).unwrap();
    assert_eq!(parsed.weight, 1.0);
    assert_eq!(serde_json::to_value(parsed).unwrap()["cell"], a.to_string());
}

#[test]
fn layer_imports_tables_with_one_aggregation_per_column() {
    let a = cell_at(LON, LAT, res(8)).unwrap();
    let specs = ["population", "income:mean=avg_income", ":count=addresses"]
        .map(|spec| ColumnSpec::parse(spec).unwrap());
    let mut builder = LayerBuilder::new(specs.to_vec()).unwrap();
    let points = format!(
        "lon,lat,population,income\n{LON},{LAT},10,30000\n{LON},{LAT},5,\n{},{LAT},7,50000\nx,y,1,1\n",
        LON + 0.2
    );
    assert_eq!(
        builder
            .add_csv(&points, Some(res(8)), "lon", "lat")
            .unwrap(),
        (3, 1)
    );
    let mut layer = CellLayer::new("city").unwrap();
    layer.merge(builder).unwrap();
    assert_eq!(layer.columns, ["population", "avg_income", "addresses"]);
    assert_eq!(layer.value(a, "population"), Some(15.0));
    assert_eq!(layer.value(a, "avg_income"), Some(30000.0));
    assert_eq!(layer.value(a, "addresses"), Some(2.0));

    // A second import adds a column from a cell table.
    let mut jobs = LayerBuilder::new(vec![ColumnSpec::parse("jobs:max").unwrap()]).unwrap();
    jobs.add_csv(&format!("cell,jobs\n{a},40\n{a},90\n"), None, "lon", "lat")
        .unwrap();
    layer.merge(jobs).unwrap();
    assert_eq!(layer.value(a, "jobs"), Some(90.0));
    assert_eq!(layer.value(a, "population"), Some(15.0));
    let summary = layer.summary();
    assert_eq!(summary.cell_count, 2);
    assert_eq!(summary.resolutions, [8]);
    assert_eq!(summary.columns[0].sum, 22.0);
    assert_eq!(summary.columns[3].count, 1);

    assert_eq!(
        default_column_specs(&points, &["lon", "lat"])
            .unwrap()
            .iter()
            .map(|spec| spec.source.as_str())
            .collect::<Vec<_>>(),
        ["population", "income"]
    );
    assert!(ColumnSpec::parse("population:median").is_err());
    assert!(CellLayer::new("bad id").is_err());
}

#[test]
fn layer_paints_and_round_trips_through_storage() {
    let a = cell_at(LON, LAT, res(8)).unwrap();
    let b = cell_at(LON + 0.2, LAT, res(8)).unwrap();
    let mut layer = CellLayer::new("demand").unwrap();
    assert_eq!(layer.paint([a, b], "riders", PaintOp::Set, 4.0).unwrap(), 2);
    assert_eq!(layer.paint([a], "riders", PaintOp::Add, 1.5).unwrap(), 1);
    assert_eq!(
        layer
            .paint([a, b], "riders", PaintOp::Multiply, 2.0)
            .unwrap(),
        2
    );
    assert_eq!(layer.paint([a], "drivers", PaintOp::Add, 3.0).unwrap(), 1);
    assert_eq!(layer.value(a, "riders"), Some(11.0));
    assert_eq!(layer.value(b, "riders"), Some(8.0));
    assert_eq!(layer.value(b, "drivers"), None);
    assert_eq!(layer.paint([b], "riders", PaintOp::Clear, 0.0).unwrap(), 1);
    assert!(
        !layer.cells.contains_key(&b),
        "a cell without values is removed"
    );
    assert!(layer.paint([a], "riders", PaintOp::Set, f64::NAN).is_err());
    assert_eq!(
        layer.weighted("riders").unwrap(),
        [WeightedCell {
            cell: a,
            weight: 11.0
        }]
    );
    assert!(layer.weighted("missing").is_err());

    let dir = std::env::temp_dir().join(format!("netweevil-h3-layer-{}", std::process::id()));
    layer.save(&dir).unwrap();
    assert_eq!(CellLayer::load(&dir, "demand").unwrap(), layer);
    assert_eq!(CellLayer::list(&dir).unwrap()[0].layer_id, "demand");
    assert!(
        layer
            .to_csv()
            .unwrap()
            .starts_with("cell,resolution,riders,drivers\n")
    );
    assert_eq!(layer.geojson()["features"][0]["properties"]["riders"], 11.0);
    assert!(CellLayer::remove(&dir, "demand").unwrap());
    assert!(!CellLayer::remove(&dir, "demand").unwrap());
    assert!(CellLayer::load(&dir, "demand").is_err());
    assert!(layer.remove_column("riders"));
    assert_eq!(layer.columns, ["drivers"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn surge_rises_where_demand_exceeds_supply() {
    let busy = cell_at(LON, LAT, res(8)).unwrap();
    let calm = cell_at(LON + 0.3, LAT, res(8)).unwrap();
    let weighted = |cell, weight| WeightedCell { cell, weight };
    let demand = [weighted(busy, 40.0), weighted(calm, 5.0)];
    let supply = [weighted(busy, 10.0), weighted(calm, 20.0)];
    let config = SurgeConfig {
        smoothing_rings: 0,
        ..SurgeConfig::default()
    };
    let cells = compute_surge(&demand, &supply, &config).unwrap();
    let of = |cell| *cells.iter().find(|row| row.cell == cell).unwrap();
    assert_eq!(of(busy).ratio, 4.0);
    assert_eq!(of(busy).multiplier, 2.5);
    assert_eq!(of(calm).multiplier, 1.0);

    // Smoothing keeps the totals and lowers the peak.
    let smoothed = compute_surge(&demand, &supply, &SurgeConfig::default()).unwrap();
    let peak = smoothed.iter().find(|row| row.cell == busy).unwrap();
    assert!(peak.smoothed_demand < 40.0 && peak.smoothed_demand > 5.0);
    assert!(peak.multiplier > 1.0);

    // Finer inputs add up in the surge cell; the cap holds without supply.
    let fine = busy
        .children(res(10))
        .take(3)
        .map(|cell| weighted(cell, 100.0));
    let capped = compute_surge(
        &fine.collect::<Vec<_>>(),
        &[],
        &SurgeConfig {
            resolution: Some(8),
            ..config.clone()
        },
    )
    .unwrap();
    assert_eq!((capped[0].cell, capped[0].demand), (busy, 300.0));
    assert_eq!(capped[0].multiplier, 3.0);

    let index = SurgeIndex::new(&cells, &config);
    assert_eq!(index.multiplier_at(LON, LAT), 2.5);
    assert_eq!(index.multiplier_at(0.0, 0.0), 1.0);

    let near = busy.grid_disk::<Vec<_>>(1)[1];
    let balance = compute_surge(
        &[weighted(busy, 12.0)],
        &[weighted(near, 5.0), weighted(calm, 50.0)],
        &config,
    )
    .unwrap();
    let moves = plan_rebalance(&balance, &config, 3);
    assert_eq!(moves.len(), 1, "the far cell is out of range");
    assert_eq!(
        (moves[0].from, moves[0].to, moves[0].amount),
        (near, busy, 5.0)
    );
    assert!(
        SurgeConfig {
            max_multiplier: 0.5,
            ..SurgeConfig::default()
        }
        .validate()
        .is_err()
    );
}
