//! Editable OSM source state: the routable ways, turn restrictions and
//! admin boundary relations of an extract, and the nodes and non-routable
//! ways they refer to.
//!
//! A PBF import reads the extract in three passes (relations, ways, then
//! only the referenced nodes), so the state is proportional to the routable
//! network rather than to the extract. It derives the [`ScanOutput`] for
//! the topology build and is persisted as the dataset's source snapshot. An
//! update restores it from the snapshot, applies change files, reads objects
//! the changes newly refer to from the imported extract and derives the
//! next scan output from the same code, so an incremental update builds the
//! topology a full import of the changed extract would build.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_core::{CacheBundleId, TemporalRuleSet, TopologyBundle};
use netweevil_persist::{
    SOURCE_SNAPSHOT_SCHEMA_VERSION, SourceSnapshot, write_source_snapshot_bundle,
};
use osmpbfreader::OsmObj;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::access::ConditionalCounts;
use crate::admin::{
    AdminBoundary, AdminRelationCandidate, assemble_boundaries, parse_admin_relation,
};
use crate::classify::is_traffic_signal_node;
use crate::guidance::{JunctionNodeTags, junction_node_tags};
use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};
use crate::osm_change::{ChangeAction, OsmChange};
use crate::pbf::{IdIndex, ObjectKind, OsmSource, PbfFile, PbfFingerprint};
use crate::restrictions::{TurnRestrictionCandidate, ViaSpec, parse_turn_restriction_relation};
use crate::scan::{
    ObjectCounts, PendingWay, RuleSetInterner, ScanOutput, is_structure_way, pending_osm_way,
};
use crate::topology::EnrichmentInputs;

/// The snapshot record types the ingest persists.
pub(crate) type OsmSourceSnapshot =
    SourceSnapshot<PendingWay, TurnRestrictionCandidate, SnapshotExtras>;

/// Classified source data the snapshot keeps beside ways and restrictions.
/// The snapshot's `other_way_*` columns hold the retained admin member
/// ways; `other_relation_ids` is empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SnapshotExtras {
    /// Guidance tags of retained motorway-junction nodes, sorted by node id.
    junction_nodes: Vec<(i64, JunctionNodeTags)>,
    /// Temporal rule sets that way and restriction records refer to, in the
    /// order a full import interns them.
    rule_sets: Vec<TemporalRuleSet>,
    /// Routable ways on bridges or in tunnels, sorted.
    structure_way_ids: Vec<i64>,
    /// Admin boundary relations, by id.
    admin_relations: Vec<AdminRelationCandidate>,
    /// Referenced nodes and admin member ways the source does not hold,
    /// sorted.
    absent_node_ids: Vec<i64>,
    absent_member_way_ids: Vec<i64>,
    /// Nodes, ways and relations in the source.
    counts: ObjectCounts,
    /// Objects change files touched since the import, sorted by id.
    changed_nodes: Vec<(i64, Option<ChangedNode>)>,
    changed_ways: Vec<(i64, Option<Vec<i64>>)>,
    /// The extract the dataset was imported from.
    base: Option<PbfFingerprint>,
    /// The boundaries file and DEM the dataset was built with; updates apply
    /// them again.
    enrichment: EnrichmentInputs,
}

impl SnapshotExtras {
    /// The enrichment inputs of the build the snapshot belongs to.
    pub(crate) fn enrichment(&self) -> &EnrichmentInputs {
        &self.enrichment
    }

    /// The extras without what change files added and without the extract
    /// fingerprint.
    #[cfg(test)]
    pub(crate) fn without_changes(mut self) -> Self {
        self.changed_nodes.clear();
        self.changed_ways.clear();
        self.base = None;
        self
    }
}

/// One source node in 1e-7 degree units, as PBF files store positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceNode {
    id: i64,
    lon_e7: i32,
    lat_e7: i32,
}

impl SourceNode {
    /// Degrees exactly as `osmpbfreader::Node::lon`/`lat` compute them.
    fn lon_lat(self) -> (f64, f64) {
        (self.lon_e7 as f64 * 1e-7, self.lat_e7 as f64 * 1e-7)
    }
}

/// A node with what its tags classify to.
#[derive(Debug, Clone, PartialEq)]
struct TaggedNode {
    node: SourceNode,
    traffic_signal: bool,
    junction: Option<JunctionNodeTags>,
}

impl TaggedNode {
    fn of(node: &osmpbfreader::Node) -> Self {
        Self {
            node: SourceNode {
                id: node.id.0,
                lon_e7: node.decimicro_lon,
                lat_e7: node.decimicro_lat,
            },
            traffic_signal: is_traffic_signal_node(&node.tags),
            junction: junction_node_tags(&node.tags),
        }
    }
}

/// A node version from a change file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChangedNode {
    lon_e7: i32,
    lat_e7: i32,
    traffic_signal: bool,
    junction: Option<JunctionNodeTags>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OsmSourceState {
    /// Nodes that routable ways, restriction via nodes and retained member
    /// ways refer to, sorted by id with unique ids.
    nodes: Vec<SourceNode>,
    /// Retained traffic-signal nodes.
    traffic_signals: FxHashSet<i64>,
    /// Guidance tags of retained motorway-junction nodes.
    junction_nodes: FxHashMap<i64, JunctionNodeTags>,
    /// Routable ways by id.
    ways: BTreeMap<i64, PendingWay>,
    /// Turn-restriction relations by id, one candidate per distinct kind,
    /// mode set and condition.
    restrictions: BTreeMap<i64, Vec<TurnRestrictionCandidate>>,
    /// Conditional access, speed and restriction rule sets, numbered as a
    /// full import of the current objects numbers them.
    rule_sets: RuleSetInterner,
    /// Node lists of the non-routable ways that admin relations list as
    /// members.
    member_ways: BTreeMap<i64, Vec<i64>>,
    /// Routable ways on bridges or in tunnels.
    structure_ways: FxHashSet<i64>,
    /// Admin boundary relations (`boundary=administrative`).
    admin_relations: BTreeMap<i64, AdminRelationCandidate>,
    /// Referenced nodes and admin member ways the source does not hold,
    /// sorted, so updates do not look for them in the extract again.
    absent_nodes: Vec<i64>,
    absent_member_ways: Vec<i64>,
    counts: ObjectCounts,
    /// Node versions from change files (`None`: deleted). They differ from
    /// the extract, so they are kept whether or not anything refers to them.
    changed_nodes: BTreeMap<i64, Option<ChangedNode>>,
    /// Ways change files made non-routable (their node lists) or deleted.
    changed_ways: BTreeMap<i64, Option<Vec<i64>>>,
    /// The extract the state was read from. Objects the state does not
    /// retain are read from it again when changes refer to them.
    base: Option<PbfFingerprint>,
}

/// Objects one change set touched, after keeping each object's newest
/// version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChangeSummary {
    pub(crate) nodes: u64,
    pub(crate) ways: u64,
    pub(crate) relations: u64,
}

/// One way of a block, classified.
enum ClassifiedWay {
    Routable {
        way: PendingWay,
        structure: bool,
    },
    /// A non-routable way; its node list when it is an admin member.
    Other {
        id: i64,
        nodes: Option<Vec<i64>>,
    },
}

/// The classified ways of one block, with the block's own rule-set
/// numbering.
#[derive(Default)]
struct WayBlock {
    ways: Vec<ClassifiedWay>,
    rule_sets: RuleSetInterner,
    conditionals: ConditionalCounts,
}

/// Fraction of the scan done after each pass, by the share of the extract
/// each pass decodes: the relation pass decodes every blob.
const PASS_ENDS: [f64; 3] = [0.4, 0.6, 1.0];

impl OsmSourceState {
    /// Reads an OSM PBF extract: relations, then ways, then the nodes the
    /// retained ways and relations refer to.
    pub(crate) fn scan_pbf(
        source_path: &Path,
        progress: &mut impl FnMut(DatasetImportProgress),
    ) -> Result<Self> {
        let mut pbf = PbfFile::open(source_path)?;
        let base = pbf.fingerprint().clone();
        let mut reporter = PercentReporter::starting_at_zero();
        emit_progress(
            progress,
            DatasetImportStage::ScanRoutableObjects,
            Some(0.0),
            "Scanning OSM objects 0%".to_string(),
        );
        let mut report = |fraction: f64| {
            reporter.emit_if_needed(
                (fraction * 1000.0) as u64,
                1000,
                DatasetImportStage::ScanRoutableObjects,
                progress,
                |percent| format!("Scanning OSM objects {percent:.0}%"),
            );
        };
        let (mut state, conditionals) = Self::scan(&mut pbf, &mut report)?;
        state.base = Some(base);
        emit_progress(
            progress,
            DatasetImportStage::ScanRoutableObjects,
            Some(100.0),
            format!(
                "Scanning OSM objects 100% ({} nodes, {} ways, {} relations; kept {} routable \
                 ways, {} restrictions, {} nodes; {} conditional tags imported, {} skipped as \
                 unsupported)",
                state.counts.nodes,
                state.counts.ways,
                state.counts.relations,
                state.ways.len(),
                state.restrictions.len(),
                state.nodes.len(),
                conditionals.imported,
                conditionals.skipped
            ),
        );
        Ok(state)
    }

    /// Builds the state from objects in source order, as a PBF scan would.
    #[cfg(test)]
    pub(crate) fn from_objects(objects: impl IntoIterator<Item = OsmObj>) -> Self {
        let mut source = crate::pbf::ObjectList(objects.into_iter().collect());
        Self::scan(&mut source, &mut |_| {})
            .expect("objects scan")
            .0
    }

    fn scan(
        source: &mut impl OsmSource,
        progress: &mut dyn FnMut(f64),
    ) -> Result<(Self, ConditionalCounts)> {
        let mut state = Self::default();
        let mut pass_progress = |pass: usize, fraction: f64| {
            let start = if pass == 0 { 0.0 } else { PASS_ENDS[pass - 1] };
            progress(start + (PASS_ENDS[pass] - start) * fraction);
        };

        source.read_pass(
            ObjectKind::Relation,
            |objects| {
                objects
                    .filter_map(|object| match object {
                        OsmObj::Relation(relation) => Some(relation),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            },
            |relations| {
                for relation in relations {
                    state.counts.relations += 1;
                    state.upsert_relation(&relation);
                }
            },
            &mut |fraction| pass_progress(0, fraction),
        )?;

        let members = IdIndex::new(state.admin_member_way_ids());
        let mut conditionals = ConditionalCounts::default();
        source.read_pass(
            ObjectKind::Way,
            |objects| classify_ways(objects, &members),
            |block| {
                state.counts.ways += block.ways.len() as u64;
                conditionals += block.conditionals;
                state.merge_way_block(block);
            },
            &mut |fraction| pass_progress(1, fraction),
        )?;
        state.absent_member_ways = members
            .ids()
            .iter()
            .copied()
            .filter(|id| !state.ways.contains_key(id) && !state.member_ways.contains_key(id))
            .collect();
        drop(members);

        let needed = IdIndex::new(state.referenced_node_ids());
        let mut found = Vec::new();
        let mut node_count = 0;
        source.read_pass(
            ObjectKind::Node,
            |objects| {
                let mut count = 0_u64;
                let mut kept = Vec::new();
                for object in objects {
                    if let OsmObj::Node(node) = object {
                        count += 1;
                        if needed.contains(node.id.0) {
                            kept.push(TaggedNode::of(&node));
                        }
                    }
                }
                (count, kept)
            },
            |(count, kept)| {
                node_count += count;
                found.extend(kept);
            },
            &mut |fraction| pass_progress(2, fraction),
        )?;
        state.counts.nodes = node_count;
        state.set_retained_nodes(found, &needed);
        state.renumber_rule_sets();
        Ok((state, conditionals))
    }

    /// Adds one classified block of ways in source order; a repeated way id
    /// replaces the earlier way.
    fn merge_way_block(&mut self, block: WayBlock) {
        let sets = block.rule_sets.into_sets();
        let mut renumbered = vec![None; sets.len()];
        for way in block.ways {
            match way {
                ClassifiedWay::Routable { mut way, structure } => {
                    let mut global = |id: u32| {
                        *renumbered[id as usize]
                            .get_or_insert_with(|| self.rule_sets.intern(sets[id as usize].clone()))
                    };
                    way.forward_temporal_rule_id = way.forward_temporal_rule_id.map(&mut global);
                    way.reverse_temporal_rule_id = way.reverse_temporal_rule_id.map(&mut global);
                    self.insert_routable_way(way, structure);
                }
                ClassifiedWay::Other { id, nodes } => {
                    self.remove_routable_way(id);
                    match nodes {
                        Some(nodes) => self.member_ways.insert(id, nodes),
                        None => self.member_ways.remove(&id),
                    };
                }
            }
        }
    }

    fn insert_routable_way(&mut self, way: PendingWay, structure: bool) {
        let id = way.osm_way_id;
        self.member_ways.remove(&id);
        if structure {
            self.structure_ways.insert(id);
        } else {
            self.structure_ways.remove(&id);
        }
        self.ways.insert(id, way);
    }

    fn remove_routable_way(&mut self, id: i64) {
        self.ways.remove(&id);
        self.structure_ways.remove(&id);
    }

    fn upsert_relation(&mut self, relation: &osmpbfreader::Relation) {
        let id = relation.id.0;
        let candidates = parse_turn_restriction_relation(relation, &mut self.rule_sets);
        if candidates.is_empty() {
            self.restrictions.remove(&id);
            match parse_admin_relation(relation) {
                Some(admin) => {
                    self.admin_relations.insert(id, admin);
                }
                None => {
                    self.admin_relations.remove(&id);
                }
            }
        } else {
            self.admin_relations.remove(&id);
            self.restrictions.insert(id, candidates);
        }
    }

    fn delete_relation(&mut self, id: i64) {
        self.restrictions.remove(&id);
        self.admin_relations.remove(&id);
    }

    /// Member ways of the admin relations that are not routable, sorted.
    fn admin_member_way_ids(&self) -> Vec<i64> {
        let mut ids = self
            .admin_relations
            .values()
            .flat_map(|candidate| candidate.member_way_ids.iter().copied())
            .filter(|id| !self.ways.contains_key(id))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Nodes of the routable ways and restriction via nodes, unsorted.
    fn routing_node_ids(&self) -> Vec<i64> {
        let mut ids = Vec::with_capacity(self.ways.values().map(|way| way.node_ids.len()).sum());
        for way in self.ways.values() {
            ids.extend_from_slice(&way.node_ids);
        }
        for candidate in self.restrictions.values().flatten() {
            if let ViaSpec::Node(node_id) = candidate.via {
                ids.push(node_id);
            }
        }
        ids
    }

    /// Every node the retained ways and restrictions refer to, unsorted.
    fn referenced_node_ids(&self) -> Vec<i64> {
        let mut ids = self.routing_node_ids();
        for nodes in self.member_ways.values() {
            ids.extend_from_slice(nodes);
        }
        ids
    }

    /// Replaces the retained nodes with `found` (in source order; a repeated
    /// id keeps its last version) and records the `needed` ids not found.
    fn set_retained_nodes(&mut self, mut found: Vec<TaggedNode>, needed: &IdIndex) {
        if !found
            .windows(2)
            .all(|pair| pair[0].node.id < pair[1].node.id)
        {
            found.sort_by_key(|tagged| tagged.node.id);
            let mut kept: Vec<TaggedNode> = Vec::with_capacity(found.len());
            for tagged in found {
                match kept.last_mut() {
                    Some(last) if last.node.id == tagged.node.id => *last = tagged,
                    _ => kept.push(tagged),
                }
            }
            found = kept;
        }
        self.traffic_signals.clear();
        self.junction_nodes.clear();
        let mut nodes = Vec::with_capacity(found.len());
        for tagged in found {
            if tagged.traffic_signal {
                self.traffic_signals.insert(tagged.node.id);
            }
            if let Some(junction) = tagged.junction {
                self.junction_nodes.insert(tagged.node.id, junction);
            }
            nodes.push(tagged.node);
        }
        let mut found_ids = nodes.iter().map(|node| node.id).peekable();
        self.absent_nodes = needed
            .ids()
            .iter()
            .copied()
            .filter(|id| {
                while found_ids.next_if(|found| found < id).is_some() {}
                found_ids.next_if_eq(id).is_none()
            })
            .collect();
        self.nodes = nodes;
    }

    /// Renumbers the rule sets in the order a full import interns them:
    /// ways by id (forward before reverse), then restriction relations by
    /// id. Drops sets nothing refers to any more.
    fn renumber_rule_sets(&mut self) {
        let sets = std::mem::take(&mut self.rule_sets).into_sets();
        let mut renumbered = RuleSetInterner::default();
        let mut renumber = |id: u32| renumbered.intern(sets[id as usize].clone());
        for way in self.ways.values_mut() {
            way.forward_temporal_rule_id = way.forward_temporal_rule_id.map(&mut renumber);
            way.reverse_temporal_rule_id = way.reverse_temporal_rule_id.map(&mut renumber);
        }
        for candidate in self.restrictions.values_mut().flatten() {
            if let Some(condition) = candidate.condition.as_mut() {
                condition.rule_set_id = renumber(condition.rule_set_id);
            }
        }
        self.rule_sets = renumbered;
    }

    fn node(&self, id: i64) -> Option<SourceNode> {
        self.nodes
            .binary_search_by_key(&id, |node| node.id)
            .ok()
            .map(|index| self.nodes[index])
    }

    /// Applies one change file's effective changes: deletions remove the
    /// object, creations and modifications replace it with the new version.
    /// Call [`Self::resolve`] after the last change file.
    pub(crate) fn apply_changes(&mut self, changes: Vec<OsmChange>) -> ChangeSummary {
        let mut summary = ChangeSummary::default();
        for change in changes {
            let deleted = change.action == ChangeAction::Delete;
            let count = |count: &mut u64| match change.action {
                ChangeAction::Create => *count += 1,
                ChangeAction::Delete => *count = count.saturating_sub(1),
                ChangeAction::Modify => {}
            };
            match change.object {
                OsmObj::Node(node) => {
                    summary.nodes += 1;
                    count(&mut self.counts.nodes);
                    let tagged = TaggedNode::of(&node);
                    self.changed_nodes.insert(
                        tagged.node.id,
                        (!deleted).then_some(ChangedNode {
                            lon_e7: tagged.node.lon_e7,
                            lat_e7: tagged.node.lat_e7,
                            traffic_signal: tagged.traffic_signal,
                            junction: tagged.junction,
                        }),
                    );
                }
                OsmObj::Way(way) => {
                    summary.ways += 1;
                    count(&mut self.counts.ways);
                    let id = way.id.0;
                    if deleted {
                        self.remove_routable_way(id);
                        self.changed_ways.insert(id, None);
                        continue;
                    }
                    let node_ids = way.nodes.iter().map(|node| node.0).collect::<Vec<_>>();
                    match pending_osm_way(
                        id,
                        node_ids.clone(),
                        &way.tags,
                        &mut self.rule_sets,
                        &mut ConditionalCounts::default(),
                    ) {
                        Some(pending) => {
                            self.changed_ways.remove(&id);
                            self.insert_routable_way(pending, is_structure_way(&way.tags));
                        }
                        None => {
                            self.remove_routable_way(id);
                            self.changed_ways.insert(id, Some(node_ids));
                        }
                    }
                }
                OsmObj::Relation(relation) => {
                    summary.relations += 1;
                    count(&mut self.counts.relations);
                    if deleted {
                        self.delete_relation(relation.id.0);
                    } else {
                        self.upsert_relation(&relation);
                    }
                }
            }
        }
        self.renumber_rule_sets();
        summary
    }

    /// Re-establishes what the state retains after changes: the member ways
    /// of the admin relations and the nodes everything refers to. Objects
    /// neither the state nor the changes hold are read from the base
    /// extract, which `open_base` opens only when one is needed.
    pub(crate) fn resolve<S: OsmSource>(
        &mut self,
        open_base: impl FnOnce() -> Result<S>,
    ) -> Result<()> {
        let mut open_base = Some(open_base);
        let mut base: Option<S> = None;
        let mut base_source = |base: &mut Option<S>| -> Result<()> {
            if base.is_none() {
                let open = open_base.take().expect("the base opens once");
                *base = Some(open()?);
            }
            Ok(())
        };

        let member_ids = self.admin_member_way_ids();
        let mut previous_members = std::mem::take(&mut self.member_ways);
        let previous_absent_ways = std::mem::take(&mut self.absent_member_ways);
        let mut missing_ways = Vec::new();
        for &id in &member_ids {
            match self.changed_ways.get(&id) {
                Some(Some(nodes)) => {
                    self.member_ways.insert(id, nodes.clone());
                }
                Some(None) => self.absent_member_ways.push(id),
                None => match previous_members.remove(&id) {
                    Some(nodes) => {
                        self.member_ways.insert(id, nodes);
                    }
                    None if previous_absent_ways.binary_search(&id).is_ok() => {
                        self.absent_member_ways.push(id);
                    }
                    None => missing_ways.push(id),
                },
            }
        }
        drop(previous_members);
        if !missing_ways.is_empty() {
            base_source(&mut base)?;
            let missing = IdIndex::new(missing_ways);
            let mut fetched = BTreeMap::new();
            base.as_mut().expect("base opened").read_pass(
                ObjectKind::Way,
                |objects| {
                    objects
                        .filter_map(|object| match object {
                            OsmObj::Way(way) if missing.contains(way.id.0) => {
                                Some((way.id.0, way.nodes.iter().map(|node| node.0).collect()))
                            }
                            _ => None,
                        })
                        .collect::<Vec<(i64, Vec<i64>)>>()
                },
                |ways| fetched.extend(ways),
                &mut |_| {},
            )?;
            for &id in missing.ids() {
                match fetched.remove(&id) {
                    Some(nodes) => {
                        self.member_ways.insert(id, nodes);
                    }
                    None => self.absent_member_ways.push(id),
                }
            }
        }
        self.absent_member_ways.sort_unstable();

        let needed = IdIndex::new(self.referenced_node_ids());
        let mut kept = Vec::with_capacity(needed.len());
        let mut missing_nodes = Vec::new();
        for &id in needed.ids() {
            match self.changed_nodes.get(&id) {
                Some(Some(changed)) => kept.push(TaggedNode {
                    node: SourceNode {
                        id,
                        lon_e7: changed.lon_e7,
                        lat_e7: changed.lat_e7,
                    },
                    traffic_signal: changed.traffic_signal,
                    junction: changed.junction.clone(),
                }),
                Some(None) => {}
                None => match self.node(id) {
                    Some(node) => kept.push(TaggedNode {
                        node,
                        traffic_signal: self.traffic_signals.contains(&id),
                        junction: self.junction_nodes.get(&id).cloned(),
                    }),
                    None if self.absent_nodes.binary_search(&id).is_ok() => {}
                    None => missing_nodes.push(id),
                },
            }
        }
        if !missing_nodes.is_empty() {
            base_source(&mut base)?;
            let missing = IdIndex::new(missing_nodes);
            let mut fetched = Vec::new();
            base.as_mut().expect("base opened").read_pass(
                ObjectKind::Node,
                |objects| {
                    objects
                        .filter_map(|object| match object {
                            OsmObj::Node(node) if missing.contains(node.id.0) => {
                                Some(TaggedNode::of(&node))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                },
                |nodes| fetched.extend(nodes),
                &mut |_| {},
            )?;
            kept.extend(fetched);
        }
        self.set_retained_nodes(kept, &needed);
        Ok(())
    }

    /// The extract the state was read from.
    pub(crate) fn base(&self) -> Option<&PbfFingerprint> {
        self.base.as_ref()
    }

    /// Writes the snapshot of this state for the topology bundle
    /// `topology_bundle_id` to `path`.
    pub(crate) fn write_snapshot(
        &self,
        path: &Path,
        topology_bundle_id: CacheBundleId,
        topology_node_ids: &[i64],
        enrichment: &EnrichmentInputs,
    ) -> Result<()> {
        write_source_snapshot_bundle(
            path,
            &self.to_snapshot(topology_bundle_id, topology_node_ids, enrichment),
        )
    }

    /// OSM ids of the topology nodes the state builds, in topology node
    /// order: the retained nodes that routable ways and via nodes use.
    pub(crate) fn topology_node_ids(&self) -> Vec<i64> {
        let routing = IdIndex::new(self.routing_node_ids());
        self.nodes
            .iter()
            .map(|node| node.id)
            .filter(|id| routing.contains(*id))
            .collect()
    }

    /// The scan output a PBF scan of this state's objects produces, for the
    /// topology nodes `topology_node_ids` (from [`Self::topology_node_ids`]).
    /// With `collect_admin`, admin boundaries are assembled from the
    /// retained admin relations and their member ways.
    pub(crate) fn into_scan_output(
        self,
        topology_node_ids: &[i64],
        collect_admin: bool,
    ) -> ScanOutput {
        let (admin_boundaries, incomplete_admin_relations) = if collect_admin {
            self.admin_boundaries()
        } else {
            (Vec::new(), Vec::new())
        };
        let mut node_coords =
            FxHashMap::with_capacity_and_hasher(topology_node_ids.len(), Default::default());
        let mut nodes = self.nodes.iter().peekable();
        for &id in topology_node_ids {
            while nodes.next_if(|node| node.id < id).is_some() {}
            let node = nodes.next().expect("topology nodes are retained nodes");
            let (lon, lat) = node.lon_lat();
            node_coords.insert(id, (lon, lat, 0.0));
        }
        let junction_nodes = self
            .junction_nodes
            .into_iter()
            .filter(|(id, _)| topology_node_ids.binary_search(id).is_ok())
            .collect();
        ScanOutput {
            pending_ways: self.ways.into_values().collect(),
            restriction_candidates: self.restrictions.into_values().flatten().collect(),
            node_coords,
            traffic_signal_nodes: self.traffic_signals,
            junction_nodes,
            counts: self.counts,
            feature_attributes: Default::default(),
            temporal_rule_sets: self.rule_sets.into_sets(),
            structure_ways: self.structure_ways,
            admin_boundaries,
            incomplete_admin_relations,
        }
    }

    /// The scan output of a copy of this state and its topology node ids.
    #[cfg(test)]
    pub(crate) fn scan_output(&self, collect_admin: bool) -> (ScanOutput, Vec<i64>) {
        let topology_node_ids = self.topology_node_ids();
        let scan = self
            .clone()
            .into_scan_output(&topology_node_ids, collect_admin);
        (scan, topology_node_ids)
    }

    /// Applies `edits` and resolves them against `open_base`.
    #[cfg(test)]
    pub(crate) fn resolve_after<S: OsmSource>(
        mut self,
        edits: Vec<crate::test_util::OsmEdit>,
        open_base: impl FnOnce() -> Result<S>,
    ) -> Result<Self> {
        let changes =
            crate::osm_change::parse_osm_change(crate::test_util::write_osc(&edits).as_bytes())?;
        self.apply_changes(crate::osm_change::effective_changes(changes));
        self.resolve(open_base)?;
        Ok(self)
    }

    /// The state without what change files added: the objects a full import
    /// of the changed extract retains.
    #[cfg(test)]
    pub(crate) fn without_changes(mut self) -> Self {
        self.changed_nodes.clear();
        self.changed_ways.clear();
        self.base = None;
        self
    }

    /// Admin boundaries assembled from the admin relations, and the ids of
    /// relations that could not be assembled.
    fn admin_boundaries(&self) -> (Vec<AdminBoundary>, Vec<i64>) {
        let candidates = self.admin_relations.values().cloned().collect::<Vec<_>>();
        let mut way_nodes = FxHashMap::<i64, Vec<i64>>::default();
        for way_id in candidates
            .iter()
            .flat_map(|candidate| candidate.member_way_ids.iter())
        {
            let nodes = self
                .ways
                .get(way_id)
                .map(|way| &way.node_ids)
                .or_else(|| self.member_ways.get(way_id));
            if let Some(nodes) = nodes {
                way_nodes.insert(*way_id, nodes.clone());
            }
        }
        let boundary_coords = way_nodes
            .values()
            .flatten()
            .filter_map(|&id| Some((id, self.node(id)?.lon_lat())))
            .collect::<FxHashMap<_, _>>();
        assemble_boundaries(&candidates, &way_nodes, &boundary_coords)
    }

    /// The persisted form of this state. `topology_node_ids` are the ids
    /// [`Self::topology_node_ids`] returned, and `enrichment` the inputs the
    /// build uses.
    pub(crate) fn to_snapshot(
        &self,
        source_topology_bundle_id: CacheBundleId,
        topology_node_ids: &[i64],
        enrichment: &EnrichmentInputs,
    ) -> OsmSourceSnapshot {
        let other_count = self.nodes.len() - topology_node_ids.len();
        let mut other_node_ids = Vec::with_capacity(other_count);
        let mut other_node_lon_e7 = Vec::with_capacity(other_count);
        let mut other_node_lat_e7 = Vec::with_capacity(other_count);
        let mut topology_ids = topology_node_ids.iter().peekable();
        for node in &self.nodes {
            if topology_ids.next_if_eq(&&node.id).is_none() {
                other_node_ids.push(node.id);
                other_node_lon_e7.push(node.lon_e7);
                other_node_lat_e7.push(node.lat_e7);
            }
        }
        let traffic_signal_node_ids = sorted(&self.traffic_signals);

        let mut names = Vec::new();
        let mut name_lookup = HashMap::<&str, u32>::new();
        let mut way_ids = Vec::with_capacity(self.ways.len());
        let mut way_node_counts = Vec::with_capacity(self.ways.len());
        let mut way_node_ids = Vec::new();
        let mut way_name_indexes = Vec::with_capacity(self.ways.len());
        let mut way_attributes = Vec::with_capacity(self.ways.len());
        for (&id, way) in &self.ways {
            way_ids.push(id);
            way_node_counts.push(way.node_ids.len() as u32);
            way_node_ids.extend_from_slice(&way.node_ids);
            way_name_indexes.push(way.name.as_deref().map(|name| {
                *name_lookup.entry(name).or_insert_with(|| {
                    names.push(name.to_string());
                    names.len() as u32 - 1
                })
            }));
            way_attributes.push(PendingWay {
                osm_way_id: 0,
                node_ids: Vec::new(),
                name: None,
                ..way.clone()
            });
        }

        SourceSnapshot {
            schema_version: SOURCE_SNAPSHOT_SCHEMA_VERSION,
            source_topology_bundle_id,
            topology_node_ids: topology_node_ids.to_vec(),
            other_node_ids,
            other_node_lon_e7,
            other_node_lat_e7,
            traffic_signal_node_ids,
            way_ids,
            way_node_counts,
            way_node_ids,
            way_name_indexes,
            names,
            way_attributes,
            restrictions: self.restrictions.values().flatten().cloned().collect(),
            other_way_ids: self.member_ways.keys().copied().collect(),
            other_way_node_counts: self
                .member_ways
                .values()
                .map(|nodes| nodes.len() as u32)
                .collect(),
            other_way_node_ids: self.member_ways.values().flatten().copied().collect(),
            other_relation_ids: Vec::new(),
            extras: SnapshotExtras {
                junction_nodes: {
                    let mut junctions = self
                        .junction_nodes
                        .iter()
                        .map(|(id, tags)| (*id, tags.clone()))
                        .collect::<Vec<_>>();
                    junctions.sort_unstable_by_key(|(id, _)| *id);
                    junctions
                },
                rule_sets: self.rule_sets.sets().to_vec(),
                structure_way_ids: sorted(&self.structure_ways),
                admin_relations: self.admin_relations.values().cloned().collect(),
                absent_node_ids: self.absent_nodes.clone(),
                absent_member_way_ids: self.absent_member_ways.clone(),
                counts: self.counts,
                changed_nodes: self
                    .changed_nodes
                    .iter()
                    .map(|(id, node)| (*id, node.clone()))
                    .collect(),
                changed_ways: self
                    .changed_ways
                    .iter()
                    .map(|(id, nodes)| (*id, nodes.clone()))
                    .collect(),
                base: self.base.clone(),
                enrichment: enrichment.clone(),
            },
        }
    }

    /// Restores the state from a snapshot and the topology built with it,
    /// which holds the topology nodes' coordinates.
    pub(crate) fn from_snapshot(
        snapshot: OsmSourceSnapshot,
        topology: &TopologyBundle,
    ) -> Result<Self> {
        if snapshot.topology_node_ids.len() != topology.nodes.len() {
            bail!(
                "source snapshot lists {} topology nodes, the topology has {}",
                snapshot.topology_node_ids.len(),
                topology.nodes.len()
            );
        }
        let topology_nodes = snapshot
            .topology_node_ids
            .iter()
            .zip(&topology.nodes)
            .map(|(&id, node)| {
                Ok(SourceNode {
                    id,
                    lon_e7: decimicro(node.lon)
                        .context("topology longitude is not an OSM position")?,
                    lat_e7: decimicro(node.lat)
                        .context("topology latitude is not an OSM position")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let other_nodes = snapshot
            .other_node_ids
            .iter()
            .zip(&snapshot.other_node_lon_e7)
            .zip(&snapshot.other_node_lat_e7)
            .map(|((&id, &lon_e7), &lat_e7)| SourceNode { id, lon_e7, lat_e7 });
        let mut nodes = Vec::with_capacity(topology_nodes.len() + snapshot.other_node_ids.len());
        let mut topology_nodes = topology_nodes.into_iter().peekable();
        for node in other_nodes {
            while let Some(topology_node) =
                topology_nodes.next_if(|candidate| candidate.id < node.id)
            {
                nodes.push(topology_node);
            }
            nodes.push(node);
        }
        nodes.extend(topology_nodes);
        if !nodes.windows(2).all(|pair| pair[0].id < pair[1].id) {
            bail!("source snapshot node ids are not unique and sorted");
        }

        let mut ways = BTreeMap::new();
        let mut node_refs = snapshot.way_node_ids.into_iter();
        for (((id, count), name_index), attributes) in snapshot
            .way_ids
            .into_iter()
            .zip(snapshot.way_node_counts)
            .zip(snapshot.way_name_indexes)
            .zip(snapshot.way_attributes)
        {
            ways.insert(
                id,
                PendingWay {
                    osm_way_id: id,
                    node_ids: node_refs.by_ref().take(count as usize).collect(),
                    name: name_index.map(|index| snapshot.names[index as usize].clone()),
                    ..attributes
                },
            );
        }
        let mut restrictions = BTreeMap::<i64, Vec<TurnRestrictionCandidate>>::new();
        for candidate in snapshot.restrictions {
            restrictions
                .entry(candidate.relation_id)
                .or_default()
                .push(candidate);
        }
        let mut member_way_refs = snapshot.other_way_node_ids.into_iter();
        let member_ways = snapshot
            .other_way_ids
            .into_iter()
            .zip(snapshot.other_way_node_counts)
            .map(|(id, count)| (id, member_way_refs.by_ref().take(count as usize).collect()))
            .collect();
        let extras = snapshot.extras;
        let mut rule_sets = RuleSetInterner::default();
        for (index, set) in extras.rule_sets.iter().enumerate() {
            if rule_sets.intern(set.clone()) as usize != index {
                bail!("source snapshot repeats a temporal rule set");
            }
        }
        Ok(Self {
            nodes,
            traffic_signals: snapshot.traffic_signal_node_ids.into_iter().collect(),
            junction_nodes: extras.junction_nodes.into_iter().collect(),
            ways,
            restrictions,
            rule_sets,
            member_ways,
            structure_ways: extras.structure_way_ids.into_iter().collect(),
            admin_relations: extras
                .admin_relations
                .into_iter()
                .map(|candidate| (candidate.relation_id, candidate))
                .collect(),
            absent_nodes: extras.absent_node_ids,
            absent_member_ways: extras.absent_member_way_ids,
            counts: extras.counts,
            changed_nodes: extras.changed_nodes.into_iter().collect(),
            changed_ways: extras.changed_ways.into_iter().collect(),
            base: extras.base,
        })
    }
}

/// Classifies one block of ways, with rule sets numbered within the block.
/// Keeps node lists of non-routable ways only for admin `members`.
fn classify_ways(objects: &mut dyn Iterator<Item = OsmObj>, members: &IdIndex) -> WayBlock {
    let mut block = WayBlock::default();
    for object in objects {
        let OsmObj::Way(way) = object else {
            continue;
        };
        let id = way.id.0;
        let node_ids = way.nodes.iter().map(|node| node.0).collect::<Vec<_>>();
        let member_nodes = members.contains(id).then(|| node_ids.clone());
        let classified = match pending_osm_way(
            id,
            node_ids,
            &way.tags,
            &mut block.rule_sets,
            &mut block.conditionals,
        ) {
            Some(pending) => ClassifiedWay::Routable {
                way: pending,
                structure: is_structure_way(&way.tags),
            },
            None => ClassifiedWay::Other {
                id,
                nodes: member_nodes,
            },
        };
        block.ways.push(classified);
    }
    block
}

/// Opens the extract at `path` that a state with base `expected` was read
/// from, after checking that it is still the same file.
pub(crate) fn open_base(path: &Path, expected: Option<&PbfFingerprint>) -> Result<PbfFile> {
    let Some(expected) = expected else {
        bail!("the source snapshot does not name its extract; re-import the dataset");
    };
    let pbf = PbfFile::open(path).with_context(|| {
        format!(
            "the update reads objects from the imported extract {}",
            path.display()
        )
    })?;
    if pbf.fingerprint() != expected {
        bail!(
            "the extract {} differs from the one the dataset was imported from; re-import \
             the dataset or update it from a complete extract",
            path.display()
        );
    }
    Ok(pbf)
}

fn sorted(ids: &FxHashSet<i64>) -> Vec<i64> {
    let mut ids = ids.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

/// The 1e-7 degree value that decodes to exactly `degrees`.
fn decimicro(degrees: f64) -> Option<i32> {
    let units = (degrees * 1e7).round();
    let units = i32::try_from(units as i64).ok()?;
    ((units as f64 * 1e-7).to_bits() == degrees.to_bits()).then_some(units)
}

#[cfg(test)]
mod tests;
