use netweevil_core::{
    AccessMask, DirectedEdge, EdgeId, NodeId, TurnRestriction, TurnRestrictionKind,
};
use osmpbfreader::{OsmId, Relation, Tags};
use std::collections::{BTreeSet, HashMap, HashSet};

use rustc_hash::FxHashMap;

use crate::classify::tag;
use crate::scan::PendingWay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestrictionKind {
    NoTurn,
    OnlyTurn,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnRestrictionCandidate {
    pub(crate) relation_id: i64,
    pub(crate) from_way_id: i64,
    pub(crate) via: ViaSpec,
    pub(crate) to_way_id: i64,
    pub(crate) kind: RestrictionKind,
    pub(crate) mode_mask: AccessMask,
}

#[derive(Debug, Clone)]
pub(crate) enum ViaSpec {
    Node(i64),
    Ways(Vec<i64>),
}

pub(crate) fn build_turn_restrictions(
    candidates: &[TurnRestrictionCandidate],
    ways: &[PendingWay],
    node_lookup: &FxHashMap<i64, (NodeId, f64, f64)>,
    edges: &[DirectedEdge],
) -> Vec<TurnRestriction> {
    let mut incoming_by_way_and_node: HashMap<(i64, NodeId), Vec<EdgeId>> = HashMap::new();
    let mut outgoing_by_way_and_node: HashMap<(i64, NodeId), Vec<EdgeId>> = HashMap::new();
    let mut edge_by_way_and_nodes: HashMap<(i64, NodeId, NodeId), EdgeId> = HashMap::new();
    let node_count = edges
        .iter()
        .flat_map(|edge| [edge.from.0 as usize, edge.to.0 as usize])
        .max()
        .map(|max_index| max_index + 1)
        .unwrap_or_default();
    let mut outgoing_by_node: Vec<Vec<EdgeId>> = vec![Vec::new(); node_count];

    for edge in edges {
        incoming_by_way_and_node
            .entry((edge.source_way_id, edge.to))
            .or_default()
            .push(edge.edge_id);
        outgoing_by_way_and_node
            .entry((edge.source_way_id, edge.from))
            .or_default()
            .push(edge.edge_id);
        edge_by_way_and_nodes.insert((edge.source_way_id, edge.from, edge.to), edge.edge_id);
        outgoing_by_node[edge.from.0 as usize].push(edge.edge_id);
    }

    let way_lookup: HashMap<_, _> = ways.iter().map(|way| (way.osm_way_id, way)).collect();
    let mut restrictions = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        match &candidate.via {
            ViaSpec::Node(via_node_id) => {
                let Some(&(via_node, _, _)) = node_lookup.get(via_node_id) else {
                    continue;
                };
                let Some(incoming_edges) =
                    incoming_by_way_and_node.get(&(candidate.from_way_id, via_node))
                else {
                    continue;
                };
                let Some(allowed_to_edges) =
                    outgoing_by_way_and_node.get(&(candidate.to_way_id, via_node))
                else {
                    continue;
                };
                expand_terminal_restrictions(
                    &mut restrictions,
                    &mut seen,
                    candidate,
                    incoming_edges,
                    &[],
                    via_node,
                    allowed_to_edges,
                    &outgoing_by_node,
                );
            }
            ViaSpec::Ways(via_way_ids) => {
                if via_way_ids.is_empty() {
                    continue;
                }
                let Some(from_way) = way_lookup.get(&candidate.from_way_id).copied() else {
                    continue;
                };
                let Some(to_way) = way_lookup.get(&candidate.to_way_id).copied() else {
                    continue;
                };
                let Some(via_ways) = via_way_ids
                    .iter()
                    .map(|way_id| way_lookup.get(way_id).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let Some(shared_nodes) = via_shared_nodes(from_way, &via_ways, to_way) else {
                    continue;
                };
                let Some(&(entry_node, _, _)) = node_lookup.get(&shared_nodes[0]) else {
                    continue;
                };
                let Some(&(exit_node, _, _)) = node_lookup.get(shared_nodes.last().unwrap()) else {
                    continue;
                };
                let Some(incoming_edges) =
                    incoming_by_way_and_node.get(&(candidate.from_way_id, entry_node))
                else {
                    continue;
                };
                let Some(allowed_to_edges) =
                    outgoing_by_way_and_node.get(&(candidate.to_way_id, exit_node))
                else {
                    continue;
                };

                let mut via_edges = Vec::new();
                let mut valid = true;
                for (way, pair) in via_ways.iter().zip(shared_nodes.windows(2)) {
                    let Some(segment_edges) = way_edge_sequence_between(
                        way,
                        pair[0],
                        pair[1],
                        node_lookup,
                        &edge_by_way_and_nodes,
                    ) else {
                        valid = false;
                        break;
                    };
                    via_edges.extend(segment_edges);
                }
                if !valid || via_edges.is_empty() {
                    continue;
                }

                expand_terminal_restrictions(
                    &mut restrictions,
                    &mut seen,
                    candidate,
                    incoming_edges,
                    &via_edges,
                    exit_node,
                    allowed_to_edges,
                    &outgoing_by_node,
                );
                if candidate.kind == RestrictionKind::OnlyTurn {
                    expand_intermediate_only_turns(
                        &mut restrictions,
                        &mut seen,
                        candidate,
                        incoming_edges,
                        &via_edges,
                        entry_node,
                        &outgoing_by_node,
                        edges,
                    );
                }
            }
        }
    }

    restrictions.sort_by_key(|restriction| {
        (
            restriction
                .edge_path
                .first()
                .map(|edge| edge.0)
                .unwrap_or_default(),
            restriction.edge_path.len(),
            restriction
                .edge_path
                .last()
                .map(|edge| edge.0)
                .unwrap_or_default(),
            restriction.mode_mask.0,
            restriction.relation_id,
        )
    });
    restrictions
}

fn expand_terminal_restrictions(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    incoming_edges: &[EdgeId],
    via_edges: &[EdgeId],
    terminal_node: NodeId,
    allowed_to_edges: &[EdgeId],
    outgoing_by_node: &[Vec<EdgeId>],
) {
    match candidate.kind {
        RestrictionKind::NoTurn => {
            for &from_edge in incoming_edges {
                for &to_edge in allowed_to_edges {
                    let mut edge_path = Vec::with_capacity(via_edges.len() + 2);
                    edge_path.push(from_edge);
                    edge_path.extend_from_slice(via_edges);
                    edge_path.push(to_edge);
                    push_restriction(restrictions, seen, candidate, edge_path);
                }
            }
        }
        RestrictionKind::OnlyTurn => {
            for &from_edge in incoming_edges {
                let mut prefix = Vec::with_capacity(via_edges.len() + 2);
                prefix.push(from_edge);
                prefix.extend_from_slice(via_edges);
                for &to_edge in &outgoing_by_node[terminal_node.0 as usize] {
                    if allowed_to_edges.contains(&to_edge) {
                        continue;
                    }
                    let mut edge_path = prefix.clone();
                    edge_path.push(to_edge);
                    push_restriction(restrictions, seen, candidate, edge_path);
                }
            }
        }
    }
}

fn expand_intermediate_only_turns(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    incoming_edges: &[EdgeId],
    via_edges: &[EdgeId],
    entry_node: NodeId,
    outgoing_by_node: &[Vec<EdgeId>],
    edges: &[DirectedEdge],
) {
    for &from_edge in incoming_edges {
        let mut prefix = vec![from_edge];
        let mut current_node = entry_node;
        for &required_edge in via_edges {
            for &other_edge in &outgoing_by_node[current_node.0 as usize] {
                if other_edge == required_edge {
                    continue;
                }
                let mut edge_path = prefix.clone();
                edge_path.push(other_edge);
                push_restriction(restrictions, seen, candidate, edge_path);
            }
            prefix.push(required_edge);
            current_node = edges[required_edge.0 as usize].to;
        }
    }
}

fn push_restriction(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    edge_path: Vec<EdgeId>,
) {
    if edge_path.len() < 2 {
        return;
    }
    let key = (
        edge_path.iter().map(|edge| edge.0).collect::<Vec<_>>(),
        candidate.mode_mask.0,
    );
    if seen.insert(key) {
        restrictions.push(TurnRestriction {
            relation_id: candidate.relation_id,
            kind: match candidate.kind {
                RestrictionKind::NoTurn => TurnRestrictionKind::NoTurn,
                RestrictionKind::OnlyTurn => TurnRestrictionKind::OnlyTurn,
            },
            edge_path,
            mode_mask: candidate.mode_mask,
        });
    }
}

fn via_shared_nodes(
    from_way: &PendingWay,
    via_ways: &[&PendingWay],
    to_way: &PendingWay,
) -> Option<Vec<i64>> {
    let mut shared = Vec::with_capacity(via_ways.len() + 1);
    let mut previous = from_way;
    for way in via_ways {
        shared.push(unique_shared_node(previous, way)?);
        previous = way;
    }
    shared.push(unique_shared_node(previous, to_way)?);
    Some(shared)
}

fn unique_shared_node(left: &PendingWay, right: &PendingWay) -> Option<i64> {
    let right_nodes = right.node_ids.iter().copied().collect::<HashSet<_>>();
    let shared = left
        .node_ids
        .iter()
        .copied()
        .filter(|node_id| right_nodes.contains(node_id))
        .collect::<BTreeSet<_>>();
    if shared.len() == 1 {
        shared.into_iter().next()
    } else {
        None
    }
}

fn way_edge_sequence_between(
    way: &PendingWay,
    start_osm_node_id: i64,
    end_osm_node_id: i64,
    node_lookup: &FxHashMap<i64, (NodeId, f64, f64)>,
    edge_by_way_and_nodes: &HashMap<(i64, NodeId, NodeId), EdgeId>,
) -> Option<Vec<EdgeId>> {
    if start_osm_node_id == end_osm_node_id {
        return None;
    }

    let start_indexes = way
        .node_ids
        .iter()
        .enumerate()
        .filter_map(|(index, node_id)| (*node_id == start_osm_node_id).then_some(index))
        .collect::<Vec<_>>();
    let end_indexes = way
        .node_ids
        .iter()
        .enumerate()
        .filter_map(|(index, node_id)| (*node_id == end_osm_node_id).then_some(index))
        .collect::<Vec<_>>();

    let mut best: Option<Vec<EdgeId>> = None;
    for start_index in &start_indexes {
        for end_index in &end_indexes {
            if start_index == end_index {
                continue;
            }
            let step: isize = if start_index < end_index { 1 } else { -1 };
            let mut cursor = *start_index as isize;
            let end = *end_index as isize;
            let mut edge_path = Vec::new();
            let mut valid = true;
            while cursor != end {
                let next = cursor + step;
                let from_osm = way.node_ids[cursor as usize];
                let to_osm = way.node_ids[next as usize];
                let Some(&(from_node, _, _)) = node_lookup.get(&from_osm) else {
                    valid = false;
                    break;
                };
                let Some(&(to_node, _, _)) = node_lookup.get(&to_osm) else {
                    valid = false;
                    break;
                };
                let Some(&edge_id) =
                    edge_by_way_and_nodes.get(&(way.osm_way_id, from_node, to_node))
                else {
                    valid = false;
                    break;
                };
                edge_path.push(edge_id);
                cursor = next;
            }

            if valid
                && !edge_path.is_empty()
                && best
                    .as_ref()
                    .is_none_or(|current| edge_path.len() < current.len())
            {
                best = Some(edge_path);
            }
        }
    }

    best
}

pub(crate) fn parse_turn_restriction_relation(
    relation: &Relation,
) -> Option<TurnRestrictionCandidate> {
    if tag(&relation.tags, "type") != Some("restriction") {
        return None;
    }

    let (kind, mode_mask) = parse_restriction_rule(&relation.tags)?;
    let mut from_way = None;
    let mut via_node = None;
    let mut via_ways = Vec::new();
    let mut to_way = None;

    for reference in &relation.refs {
        match (reference.role.as_str(), reference.member) {
            ("from", OsmId::Way(way_id)) => assign_unique(&mut from_way, way_id.0)?,
            ("via", OsmId::Node(node_id)) => assign_unique(&mut via_node, node_id.0)?,
            ("via", OsmId::Way(way_id)) => via_ways.push(way_id.0),
            ("to", OsmId::Way(way_id)) => assign_unique(&mut to_way, way_id.0)?,
            _ => {}
        }
    }

    let via = match (via_node, via_ways.is_empty()) {
        (Some(node_id), true) => ViaSpec::Node(node_id),
        (None, false) => ViaSpec::Ways(via_ways),
        _ => return None,
    };

    Some(TurnRestrictionCandidate {
        relation_id: relation.id.0,
        from_way_id: from_way?,
        via,
        to_way_id: to_way?,
        kind,
        mode_mask,
    })
}

fn parse_restriction_rule(tags: &Tags) -> Option<(RestrictionKind, AccessMask)> {
    let mut kind = None;
    let mut mode_bits = 0_u16;

    for (key, default_mask) in [
        (
            "restriction",
            AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        (
            "restriction:motor_vehicle",
            AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        (
            "restriction:vehicle",
            AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        ("restriction:motorcar", AccessMask::CAR),
        ("restriction:motorcycle", AccessMask::CAR),
        ("restriction:moped", AccessMask::CAR),
        ("restriction:hgv", AccessMask::HGV),
        ("restriction:goods", AccessMask::HGV),
        ("restriction:bus", AccessMask::TRANSIT),
        ("restriction:psv", AccessMask::TRANSIT),
        ("restriction:taxi", AccessMask::TRANSIT),
        ("restriction:bicycle", AccessMask::BICYCLE),
        ("restriction:foot", AccessMask::FOOT),
    ] {
        let Some(value) = tag(tags, key) else {
            continue;
        };
        let parsed_kind = parse_restriction_kind(value)?;
        if let Some(existing_kind) = kind {
            if existing_kind != parsed_kind {
                return None;
            }
        } else {
            kind = Some(parsed_kind);
        }
        mode_bits |= default_mask;
    }

    mode_bits &= !parse_except_modes(tag(tags, "except"));
    if mode_bits == 0 {
        return None;
    }
    Some((kind?, AccessMask::new(mode_bits)))
}

fn parse_restriction_kind(value: &str) -> Option<RestrictionKind> {
    let value = value.trim();
    if value.starts_with("no_") {
        Some(RestrictionKind::NoTurn)
    } else if value.starts_with("only_") {
        Some(RestrictionKind::OnlyTurn)
    } else {
        None
    }
}

fn parse_except_modes(except: Option<&str>) -> u16 {
    let mut bits = 0_u16;
    let Some(except) = except else {
        return bits;
    };

    for mode in except
        .split([';', ','])
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        bits |= restriction_mode_bits(mode);
    }

    bits
}

fn restriction_mode_bits(mode: &str) -> u16 {
    match mode {
        "motorcar" | "motor_vehicle:conditional" => AccessMask::CAR,
        "motor_vehicle" => AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        "motorcycle" | "moped" => AccessMask::CAR,
        "vehicle" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT,
        "hgv" | "goods" => AccessMask::HGV,
        "bus" | "psv" | "taxi" => AccessMask::TRANSIT,
        "bicycle" => AccessMask::BICYCLE,
        "foot" | "pedestrian" => AccessMask::FOOT,
        _ => 0,
    }
}

fn assign_unique(slot: &mut Option<i64>, value: i64) -> Option<()> {
    match slot {
        Some(existing) if *existing != value => None,
        Some(_) => Some(()),
        None => {
            *slot = Some(value);
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RestrictionKind, TurnRestrictionCandidate, ViaSpec, build_turn_restrictions,
        parse_turn_restriction_relation,
    };
    use crate::test_util::{edge, pending_way};
    use netweevil_core::{AccessMask, EdgeId, NodeId, TurnRestrictionKind};
    use osmpbfreader::{NodeId as OsmNodeId, OsmId, Ref, Relation, RelationId, Tags, WayId};
    use std::collections::HashMap;

    #[test]
    fn parses_node_based_turn_restriction_relations() {
        let relation = Relation {
            id: RelationId(42),
            tags: Tags::from_iter([
                ("type".into(), "restriction".into()),
                ("restriction".into(), "no_left_turn".into()),
            ]),
            refs: vec![
                Ref {
                    member: OsmId::Way(WayId(10)),
                    role: "from".into(),
                },
                Ref {
                    member: OsmId::Node(OsmNodeId(20)),
                    role: "via".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(30)),
                    role: "to".into(),
                },
            ],
        };

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
    fn expands_only_turn_relations_into_prohibited_transitions() {
        let candidates = vec![TurnRestrictionCandidate {
            relation_id: 7,
            from_way_id: 10,
            via: ViaSpec::Node(100),
            to_way_id: 11,
            kind: RestrictionKind::OnlyTurn,
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        let ways = vec![];
        let node_lookup = super::FxHashMap::from_iter([(100_i64, (NodeId(1), 0.0, 0.0))]);
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 1, 3, 12)];

        let restrictions = build_turn_restrictions(&candidates, &ways, &node_lookup, &edges);
        assert_eq!(restrictions.len(), 1);
        assert_eq!(restrictions[0].kind, TurnRestrictionKind::OnlyTurn);
        assert_eq!(restrictions[0].edge_path, vec![EdgeId(0), EdgeId(2)]);
    }

    #[test]
    fn parses_via_way_turn_restriction_relations() {
        let relation = Relation {
            id: RelationId(43),
            tags: Tags::from_iter([
                ("type".into(), "restriction".into()),
                ("restriction:vehicle".into(), "no_straight_on".into()),
            ]),
            refs: vec![
                Ref {
                    member: OsmId::Way(WayId(10)),
                    role: "from".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(20)),
                    role: "via".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(30)),
                    role: "to".into(),
                },
            ],
        };

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
    fn expands_via_way_only_turns_into_multi_edge_prohibitions() {
        let candidates = vec![TurnRestrictionCandidate {
            relation_id: 8,
            from_way_id: 10,
            via: ViaSpec::Ways(vec![20]),
            to_way_id: 30,
            kind: RestrictionKind::OnlyTurn,
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        let ways = vec![
            pending_way(10, &[1, 2]),
            pending_way(20, &[2, 3, 4]),
            pending_way(30, &[4, 5]),
        ];
        let node_lookup = super::FxHashMap::from_iter([
            (1_i64, (NodeId(0), 0.0, 0.0)),
            (2_i64, (NodeId(1), 0.0, 0.0)),
            (3_i64, (NodeId(2), 0.0, 0.0)),
            (4_i64, (NodeId(3), 0.0, 0.0)),
            (5_i64, (NodeId(4), 0.0, 0.0)),
            (6_i64, (NodeId(5), 0.0, 0.0)),
        ]);
        let edges = vec![
            edge(0, 0, 1, 10),
            edge(1, 1, 2, 20),
            edge(2, 2, 3, 20),
            edge(3, 3, 4, 30),
            edge(4, 1, 5, 99),
            edge(5, 2, 5, 98),
            edge(6, 3, 5, 97),
        ];

        let restrictions = build_turn_restrictions(&candidates, &ways, &node_lookup, &edges);
        assert_eq!(restrictions.len(), 3);
        assert_eq!(restrictions[0].edge_path, vec![EdgeId(0), EdgeId(4)]);
        assert_eq!(
            restrictions[1].edge_path,
            vec![EdgeId(0), EdgeId(1), EdgeId(5)]
        );
        assert_eq!(
            restrictions[2].edge_path,
            vec![EdgeId(0), EdgeId(1), EdgeId(2), EdgeId(6)]
        );
    }
}
