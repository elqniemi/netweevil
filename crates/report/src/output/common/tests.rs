use std::collections::BTreeMap;

use super::{coerce_linestring_coords_z, component_columns, csv_escape, linestring_wkt_z};

#[test]
fn formats_three_dimensional_linestrings() {
    assert_eq!(
        coerce_linestring_coords_z(&[[1.0, 2.0, 3.0]]),
        vec![[1.0, 2.0, 3.0], [1.0, 2.0, 3.0]]
    );
    assert_eq!(
        linestring_wkt_z(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
        "LINESTRING Z(1 2 3, 4 5 6)"
    );
}

#[test]
fn escapes_csv_fields() {
    assert_eq!(csv_escape("plain"), "plain");
    assert_eq!(csv_escape("a,b"), "\"a,b\"");
    assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
}

#[test]
fn component_columns_are_stable_sanitized_and_collision_safe() {
    let components = BTreeMap::from([
        ("Slope Cost".to_string(), 1.0),
        ("slope-cost".to_string(), 2.0),
        ("日照".to_string(), 3.0),
    ]);
    let columns = component_columns([&components]);
    assert_eq!(
        columns
            .iter()
            .map(|column| column.column_name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "component_slope_cost",
            "component_slope_cost_2",
            "component_unnamed"
        ]
    );
}
