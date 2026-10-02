use super::{
    FxHashMap, RestrictionKind, TurnRestrictionCandidate, ViaSpec, build_turn_restrictions,
    parse_turn_restriction_relation,
};
use crate::scan::RuleSetInterner;
use crate::test_util::{edge, pending_way};
use netweevil_core::{AccessMask, EdgeId, NodeId, TemporalEffect, TurnRestrictionKind};
use osmpbfreader::{NodeId as OsmNodeId, OsmId, Ref, Relation, RelationId, Tags, WayId};

fn relation(id: i64, tags: &[(&str, &str)], via: OsmId) -> Relation {
    let member = |member, role: &str| Ref {
        member,
        role: role.into(),
    };
    Relation {
        id: RelationId(id),
        tags: Tags::from_iter(
            std::iter::once(("type".into(), "restriction".into()))
                .chain(tags.iter().map(|&(key, value)| (key.into(), value.into()))),
        ),
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
        condition: None,
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
fn parses_node_and_via_way_relation_members() {
    let mut rule_sets = RuleSetInterner::default();
    let node = relation(
        42,
        &[("restriction", "no_left_turn")],
        OsmId::Node(OsmNodeId(20)),
    );
    let [candidate] = parse_turn_restriction_relation(&node, &mut rule_sets)
        .try_into()
        .expect("one candidate");
    assert_eq!(candidate.relation_id, 42);
    assert_eq!(candidate.from_way_id, 10);
    assert!(matches!(candidate.via, ViaSpec::Node(20)));
    assert_eq!(candidate.to_way_id, 30);
    assert_eq!(candidate.kind, RestrictionKind::NoTurn);
    assert!(candidate.condition.is_none());

    let via_way = relation(
        43,
        &[("restriction:vehicle", "no_straight_on")],
        OsmId::Way(WayId(20)),
    );
    let [candidate] = parse_turn_restriction_relation(&via_way, &mut rule_sets)
        .try_into()
        .expect("one candidate");
    assert!(matches!(candidate.via, ViaSpec::Ways(ref ways) if ways == &vec![20]));
}

#[test]
fn maps_restriction_keys_and_exceptions_onto_modes() {
    use AccessMask as A;
    let cases: Vec<(Vec<(&str, &str)>, u16)> = vec![
        (vec![("restriction", "no_left_turn")], A::MOTOR_VEHICLES),
        (vec![("restriction:vehicle", "no_u_turn")], A::VEHICLES),
        (vec![("restriction:hgv", "no_right_turn")], A::HGV),
        (
            vec![("restriction:psv", "only_straight_on")],
            A::BUS | A::TAXI,
        ),
        (
            vec![("restriction:motorcycle", "no_left_turn")],
            A::MOTORCYCLE,
        ),
        (vec![("restriction:moped", "no_left_turn")], A::MOPED),
        (
            vec![("restriction", "no_left_turn"), ("except", "psv;moped")],
            A::CAR | A::HGV | A::MOTORCYCLE,
        ),
        (
            vec![
                ("restriction", "no_left_turn"),
                ("restriction:bicycle", "no_left_turn"),
            ],
            A::MOTOR_VEHICLES | A::BICYCLE,
        ),
    ];
    for (tags, expected) in cases {
        let candidates = parse_turn_restriction_relation(
            &relation(1, &tags, OsmId::Node(OsmNodeId(20))),
            &mut RuleSetInterner::default(),
        );
        assert_eq!(candidates.len(), 1, "{tags:?}");
        assert_eq!(
            candidates[0].mode_mask,
            AccessMask::new(expected),
            "{tags:?}"
        );
    }
    // Conflicting kinds and non-restriction values are rejected.
    for tags in [
        vec![
            ("restriction", "no_left_turn"),
            ("restriction:hgv", "only_straight_on"),
        ],
        vec![("restriction", "give_way")],
        vec![("restriction", "no_left_turn"), ("except", "motor_vehicle")],
    ] {
        assert!(
            parse_turn_restriction_relation(
                &relation(1, &tags, OsmId::Node(OsmNodeId(20))),
                &mut RuleSetInterner::default(),
            )
            .is_empty(),
            "{tags:?}"
        );
    }
}

#[test]
fn parses_time_conditional_and_lifted_restrictions() {
    use AccessMask as A;
    // (tags, [(modes, applies statically, condition effect)])
    let cases: Vec<(
        Vec<(&str, &str)>,
        Vec<(u16, Option<(bool, TemporalEffect)>)>,
    )> = vec![
        (
            vec![(
                "restriction:conditional",
                "no_left_turn @ (Mo-Fr 07:00-09:00)",
            )],
            vec![(A::MOTOR_VEHICLES, Some((false, TemporalEffect::Closed)))],
        ),
        (
            vec![
                ("restriction", "no_right_turn"),
                ("restriction:conditional", "none @ (Sa-Su)"),
            ],
            vec![(A::MOTOR_VEHICLES, Some((true, TemporalEffect::OpenOnly)))],
        ),
        (
            vec![
                ("restriction", "no_right_turn"),
                ("restriction:bus:conditional", "none @ (06:00-20:00)"),
            ],
            vec![
                (A::MOTOR_VEHICLES & !A::BUS, None),
                (A::BUS, Some((true, TemporalEffect::OpenOnly))),
            ],
        ),
        (
            vec![
                ("restriction:hgv", "no_left_turn"),
                ("restriction:conditional", "no_left_turn @ (22:00-06:00)"),
            ],
            vec![
                (A::HGV, None),
                (
                    A::MOTOR_VEHICLES & !A::HGV,
                    Some((false, TemporalEffect::Closed)),
                ),
            ],
        ),
        // Sun-relative conditions are unsupported: the static tag stands.
        (
            vec![
                ("restriction", "no_left_turn"),
                ("restriction:conditional", "none @ (sunset-sunrise)"),
            ],
            vec![(A::MOTOR_VEHICLES, None)],
        ),
    ];
    for (tags, expected) in cases {
        let mut rule_sets = RuleSetInterner::default();
        let candidates = parse_turn_restriction_relation(
            &relation(1, &tags, OsmId::Node(OsmNodeId(20))),
            &mut rule_sets,
        );
        let sets = rule_sets.into_sets();
        let actual = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.mode_mask.0,
                    candidate.condition.map(|condition| {
                        let rules = &sets[condition.rule_set_id as usize].rules;
                        assert!(rules.iter().all(|rule| rule.effect == rules[0].effect));
                        (condition.applies_statically, rules[0].effect)
                    }),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{tags:?}");
    }
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
