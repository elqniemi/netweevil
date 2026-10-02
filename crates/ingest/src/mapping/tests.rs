use super::*;

#[test]
fn parses_compact_and_detailed_field_mappings() {
    let mapping: GeoPackageMapping = serde_yaml::from_str(
        r#"
retain: all
layers:
  - table: walkways
    fields:
      id: feature_id
      direction:
        role: direction
        decode: {"-1": reverse, "0": both, "1": forward}
"#,
    )
    .expect("mapping");
    mapping.validate().expect("valid mapping");
    assert_eq!(mapping.layers[0].fields["id"].role(), "feature_id");
    assert_eq!(
        mapping.layers[0].fields["direction"]
            .decode()
            .and_then(|decode| decode.get("-1"))
            .map(String::as_str),
        Some("reverse")
    );
}

#[test]
fn rejects_a_layer_that_would_materialize_no_edges() {
    let mapping: GeoPackageMapping = serde_yaml::from_str(
        r#"
retain: all
layers:
  - table: walkways
    defaults: {access: []}
    fields: {id: feature_id}
"#,
    )
    .expect("mapping parses");
    let error = mapping
        .validate()
        .expect_err("empty access would make geometry unauditable");
    assert!(error.to_string().contains("empty defaults.access"));
}
