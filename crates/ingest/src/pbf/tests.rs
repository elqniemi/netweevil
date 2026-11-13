use super::*;
use crate::test_util::{TempDir, osm_grid, osm_node, write_pbf};

fn pass(pbf: &mut PbfFile, kind: ObjectKind) -> Vec<OsmObj> {
    let mut objects = Vec::new();
    pbf.read_pass(
        kind,
        |objects| objects.collect::<Vec<_>>(),
        |block| objects.extend(block),
        &mut |_| {},
    )
    .expect("pass reads");
    objects
}

/// Several blocks of nodes, then ways and relations.
fn many_objects() -> Vec<OsmObj> {
    let mut objects = (1..=20_000)
        .map(|id| osm_node(id, 65_000_000 + id as i32, 532_000_000 - id as i32, &[]))
        .collect::<Vec<_>>();
    objects.extend(
        osm_grid()
            .into_iter()
            .filter(|object| !matches!(object, OsmObj::Node(_))),
    );
    objects
}

#[test]
fn passes_yield_each_kind_in_file_order() {
    let dir = TempDir::new("pbf-passes");
    let path = dir.join("objects.osm.pbf");
    let objects = many_objects();
    write_pbf(&path, &objects);
    let mut reference = OsmPbfReader::new(File::open(&path).unwrap());
    let all = reference
        .iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("reference reads");
    assert_eq!(all, objects);

    let mut pbf = PbfFile::open(&path).expect("PBF indexes");
    assert!(pbf.blobs.len() > 3);
    for kind in [ObjectKind::Relation, ObjectKind::Way, ObjectKind::Node] {
        let expected = all
            .iter()
            .filter(|object| ObjectKind::of(object) == kind)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(pass(&mut pbf, kind), expected, "{kind:?}");
    }
    // The first pass learned what every blob holds.
    assert!(pbf.blobs.iter().all(|blob| blob.kinds.count_ones() == 1));
}

#[test]
fn fingerprint_tells_extracts_apart() {
    let dir = TempDir::new("pbf-fingerprint");
    let (first, second) = (dir.join("a.osm.pbf"), dir.join("b.osm.pbf"));
    write_pbf(&first, &osm_grid());
    let mut edited = osm_grid();
    edited.pop();
    write_pbf(&second, &edited);
    let fingerprint = |path: &Path| PbfFile::open(path).unwrap().fingerprint().clone();
    assert_eq!(fingerprint(&first), fingerprint(&first));
    assert_ne!(fingerprint(&first), fingerprint(&second));
}

#[test]
fn truncated_files_are_rejected() {
    let dir = TempDir::new("pbf-truncated");
    let path = dir.join("cut.osm.pbf");
    write_pbf(&path, &osm_grid());
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 10]).unwrap();
    assert!(PbfFile::open(&path).is_err());
}

#[test]
fn id_index_finds_positions_in_every_range() {
    let ids = vec![5, -3, 1 << 40, 70_000, 5, (1 << 37) - 1, 0, 65_535, 65_536];
    let index = IdIndex::new(ids);
    assert_eq!(
        index.ids(),
        [-3, 0, 5, 65_535, 65_536, 70_000, (1 << 37) - 1, 1 << 40]
    );
    for (position, &id) in index.ids().iter().enumerate() {
        assert_eq!(index.position(id), Some(position), "{id}");
    }
    for absent in [-4, 1, 65_537, 1 << 37, i64::MAX, i64::MIN] {
        assert!(!index.contains(absent), "{absent}");
    }
    assert!(!IdIndex::new(Vec::new()).contains(0));
    assert!(IdIndex::new(vec![-5, -2]).contains(-2));
    let extreme = IdIndex::new(vec![i64::MAX, 7]);
    assert_eq!(extreme.position(i64::MAX), Some(1));
    // Dense and sparse ranges both bucket.
    let dense = IdIndex::new((0..100_000).map(|id| id * 3).collect());
    assert!(dense.contains(299_997) && !dense.contains(299_998));
    assert!(dense.buckets.len() < dense.len() / 8);
}
