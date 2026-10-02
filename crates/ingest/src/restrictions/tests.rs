use super::{
    FxHashMap, RestrictionKind, TurnRestrictionCandidate, ViaSpec, build_turn_restrictions,
    parse_turn_restriction_relation,
};
use crate::test_util::{edge, pending_way};
use netweevil_core::{AccessMask, EdgeId, NodeId, TurnRestrictionKind};
use osmpbfreader::{NodeId as OsmNodeId, OsmId, Ref, Relation, RelationId, Tags, WayId};

fn relation(id: i64, restriction: (&str, &str), via: OsmId) -> Relation {
    let member = |member, role: &str| Ref {
        member,
        role: role.into(),
    };
    Relation {
        id: RelationId(id),
        tags: Tags::from_iter([
            ("type".into(), "restriction".into()),
            (restriction.0.into(), restriction.1.into()),
        ]),
        refs: vec![
            member(OsmId::Way(WayId(10)), "from"),
            member(via, "via"),
            member(OsmId::Way(WayId(30)), "to"),
        ],
    }
}

fn only_turn(
    relation_id: i64,
    from_way_id: i64,
    via: ViaSpec,
    to_way_id: i64,
) -> TurnRestrictionCandidate {
    TurnRestrictionCandidate {
        relation_id,
        from_way_id,
        via,
        to_way_id,
        kind: RestrictionKind::OnlyTurn,
        mode_mask: AccessMask::new(AccessMask::CAR),
    }
}

/// Maps OSM node `osm_id` to graph node `index` for each listed id.
fn node_lookup(osm_ids: &[i64]) -> FxHashMap<i64, (NodeId, f64, f64, f64)> {
    osm_ids
        .iter()
        .enumerate()
        .map(|(index, &osm_id)| (osm_id, (NodeId(index as u32), 0.0, 0.0, 0.0)))
        .collect()
}

#[test]
fn parses_node_based_turn_restriction_relations() {
    let relation = relation(
        42,
        ("restriction", "no_left_turn"),
        OsmId::Node(OsmNodeId(20)),
    );

    let candidate = parse_turn_restriction_relation(&relation).expect("relation parses");
    assert_eq!(candidate.relation_id, 42);
    assert_eq!(candidate.from_way_id, 10);
    assert!(matches!(candidate.via, ViaSpec::Node(20)));
    assert_eq!(candidate.to_way_id, 30);
    assert_eq!(candidate.kind, RestrictionKind::NoTurn);
    assert_eq!(
        candidate.mode_mask,
        AccessMask::new(AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT)
    );
}

#[test]
fn parses_via_way_turn_restriction_relations() {
    let relation = relation(
        43,
        ("restriction:vehicle", "no_straight_on"),
        OsmId::Way(WayId(20)),
    );

    let candidate = parse_turn_restriction_relation(&relation).expect("relation parses");
    assert!(matches!(candidate.via, ViaSpec::Ways(ref ways) if ways == &vec![20]));
    assert_eq!(
        candidate.mode_mask,
        AccessMask::new(
            AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT
        )
    );
}

#[test]
fn expands_only_turn_relations_into_prohibited_transitions() {
    let candidates = [only_turn(7, 10, ViaSpec::Node(100), 11)];
    let mut node_lookup = FxHashMap::default();
    node_lookup.insert(100_i64, (NodeId(1), 0.0, 0.0, 0.0));
    let edges = [edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 1, 3, 12)];

    let restrictions = build_turn_restrictions(&candidates, &[], &node_lookup, &edges);
    assert_eq!(restrictions.len(), 1);
    assert_eq!(restrictions[0].kind, TurnRestrictionKind::OnlyTurn);
    assert_eq!(restrictions[0].edge_path, vec![EdgeId(0), EdgeId(2)]);
}

#[test]
fn expands_via_way_only_turns_into_multi_edge_prohibitions() {
    let candidates = [only_turn(8, 10, ViaSpec::Ways(vec![20]), 30)];
    let ways = [
        pending_way(10, &[1, 2]),
        pending_way(20, &[2, 3, 4]),
        pending_way(30, &[4, 5]),
    ];
    // Ways 97..99 branch off the from/via chain at every node, so each
    // departure from the mandated path becomes its own prohibition.
    let edges = [
        edge(0, 0, 1, 10),
        edge(1, 1, 2, 20),
        edge(2, 2, 3, 20),
        edge(3, 3, 4, 30),
        edge(4, 1, 5, 99),
        edge(5, 2, 5, 98),
        edge(6, 3, 5, 97),
    ];

    let restrictions = build_turn_restrictions(
        &candidates,
        &ways,
        &node_lookup(&[1, 2, 3, 4, 5, 6]),
        &edges,
    );
    let paths = restrictions
        .iter()
        .map(|restriction| restriction.edge_path.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            vec![EdgeId(0), EdgeId(4)],
            vec![EdgeId(0), EdgeId(1), EdgeId(5)],
            vec![EdgeId(0), EdgeId(1), EdgeId(2), EdgeId(6)],
        ]
    );
}
