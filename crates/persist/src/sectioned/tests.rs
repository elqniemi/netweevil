use super::*;
use netweevil_core::EdgePresentation;

fn push_bincode<T: Serialize>(bytes: &mut Vec<u8>, value: &T) {
    let section = bincode::serialize(value).expect("section serializes");
    bytes.extend_from_slice(&(section.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&section);
}

fn push_raw<T: bytemuck::Pod>(bytes: &mut Vec<u8>, value: &[T]) {
    let section = bytemuck::cast_slice(value);
    bytes.extend_from_slice(&(section.len() as u64).to_le_bytes());
    bytes.extend_from_slice(section);
}

/// Builds a topology bundle whose header carries `schema_version`.
fn topology_bytes(magic: &[u8; 8], schema_version: u32) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    push_bincode(&mut bytes, &"topology");
    push_bincode(
        &mut bytes,
        &TopologyHeader {
            schema_version,
            source_path: "city.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            turn_restrictions: Vec::new(),
            names: Vec::new(),
            spatial_index: None,
        },
    );
    push_raw(&mut bytes, &[0_u32, 1]);
    push_raw(&mut bytes, &[114.1_f64, 114.2]);
    push_raw(&mut bytes, &[22.3_f64, 22.4]);
    push_raw(&mut bytes, &[0.0_f64, 0.0]);
    push_raw(&mut bytes, &[0_u32]);
    push_raw(&mut bytes, &[0_u32]);
    push_raw(&mut bytes, &[1_u32]);
    push_raw(&mut bytes, &[42_i64]);
    push_raw(&mut bytes, &[100_u32]);
    push_raw(&mut bytes, &[0.0_f32]);
    push_raw(&mut bytes, &[0.0_f32]);
    push_raw(&mut bytes, &[0_u32]);
    push_raw(&mut bytes, &[netweevil_core::NO_FEATURE_ROW]);
    push_raw(&mut bytes, &[0_i8]);
    push_raw(&mut bytes, &[u32::MAX]);
    push_bincode(&mut bytes, &vec![EdgeProfileAttributes::default()]);
    push_bincode(&mut bytes, &vec![EdgePresentation::default()]);
    for _ in 0..6 {
        push_raw::<u32>(&mut bytes, &[]);
    }
    push_bincode(
        &mut bytes,
        &netweevil_core::FeatureAttributeTable::default(),
    );
    push_bincode(&mut bytes, &Vec::<netweevil_core::TemporalRuleSet>::new());
    bytes
}

#[test]
fn reads_a_current_topology_bundle() {
    let bytes = topology_bytes(CURRENT_MAGIC, TOPOLOGY_BUNDLE_SCHEMA_VERSION);
    let bundle =
        read_topology_sectioned(&bytes, Path::new("topology.bin")).expect("current topology reads");
    assert_eq!(bundle.nodes.len(), 2);
    assert_eq!(bundle.edge_count(), 1);
    assert_eq!(bundle.routing_edge(0).length_m, 100);
}

#[test]
fn rejects_an_unrecognized_magic_or_schema_version() {
    for (magic, schema_version) in [
        (b"INVALID!", TOPOLOGY_BUNDLE_SCHEMA_VERSION),
        (CURRENT_MAGIC, TOPOLOGY_BUNDLE_SCHEMA_VERSION - 1),
    ] {
        let bytes = topology_bytes(magic, schema_version);
        let error = read_topology_sectioned(&bytes, Path::new("topology.bin"))
            .expect_err("stale or foreign bundles are rejected");
        assert!(
            error.to_string().contains("re-import the dataset"),
            "{error}"
        );
    }
}
