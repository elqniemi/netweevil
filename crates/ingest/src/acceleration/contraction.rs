//! Contraction of the edge-state graph in a fixed elimination order.
//!
//! Contracting state `v` inserts a shortcut `x -> y` for every pair of arcs
//! `x -> v` and `v -> y` whose other endpoints outrank `v`. Every arc is
//! owned by its lower-ranked endpoint, and an owner's row is final once the
//! owner is contracted, since only lower-ranked states add arcs to it.
//!
//! Contracting `v` only adds arcs between neighbours of `v` that outrank it.
//! In the undirected fill-in, which contains every directed arc, all of them
//! are ancestors of `v` in the elimination tree, so states that are not
//! ancestors of one another never touch each other's rows. Small subtrees of
//! the elimination tree are contracted one per task. The few states above
//! them are grouped by height (the longest tree path down to a leaf) and
//! contracted lowest height first, the states of one height in parallel.
//! Rows are sets, so the result does not depend on the order in which arcs
//! reach them.

use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Result, bail};
use netweevil_core::TopologyBundle;
use rayon::prelude::*;

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter};
use crate::topology::checked_u32;

/// Subtree tasks per thread, so that uneven subtrees still balance.
const SUBTREES_PER_THREAD: usize = 8;
/// Subtrees up to this size are always one task.
const MIN_SUBTREE_STATES: usize = 1024;
/// Number of incoming-outgoing pairs from which a top state generates its
/// shortcuts in parallel.
const PARALLEL_PAIRS: usize = 1 << 14;
/// Pending rows shorter than this grow without dropping repeated keys.
const COMPACT_MIN_KEYS: usize = 32;
/// Marks a state without elimination-tree parent or ancestor.
const NO_STATE: u32 = u32::MAX;
/// Edge states are addressed by 31-bit ids so that an arc key fits 32 bits.
const MAX_EDGE_STATES: usize = 1 << 31;

/// Upward and downward CCH rows in CSR form plus arc counts.
pub(super) struct Contraction {
    pub(super) base_arc_count: u64,
    pub(super) shortcut_arc_count: u64,
    pub(super) upward_first_out: Vec<u32>,
    pub(super) upward_head: Vec<u32>,
    pub(super) downward_first_out: Vec<u32>,
    pub(super) downward_head: Vec<u32>,
}

/// An arc seen from its owner: `(other_endpoint << 1) | outgoing`.
fn key(other: u32, outgoing: bool) -> u32 {
    (other << 1) | u32::from(outgoing)
}

/// Shortcuts handed to a state that is not contracted yet, as unsorted and
/// possibly repeated keys. Rows start without allocation, so the thread that
/// contracts a subtree also allocates and frees the rows inside it.
type PendingRow = Mutex<Vec<u32>>;

fn lock(row: &PendingRow) -> MutexGuard<'_, Vec<u32>> {
    row.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Appends `keys` to a pending row, dropping repeated keys first when the
/// row would otherwise grow its allocation.
fn append(row: &PendingRow, keys: &[u32]) {
    let mut row = lock(row);
    if row.len() >= COMPACT_MIN_KEYS && row.len() + keys.len() > row.capacity() {
        row.sort_unstable();
        row.dedup();
    }
    row.extend_from_slice(keys);
}

/// Contracts every edge state of `topology` in `edge_order`
/// (`edge_rank[edge_order[rank]] == rank`).
pub(super) fn contract(
    topology: &TopologyBundle,
    edge_order: &[u32],
    edge_rank: &[u32],
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<Contraction> {
    let subtree_limit = (edge_rank.len() / (rayon::current_num_threads() * SUBTREES_PER_THREAD))
        .max(MIN_SUBTREE_STATES);
    contract_with_subtree_limit(topology, edge_order, edge_rank, subtree_limit, progress)
}

/// [`contract`] with subtrees of at most `subtree_limit` states as tasks.
pub(super) fn contract_with_subtree_limit(
    topology: &TopologyBundle,
    edge_order: &[u32],
    edge_rank: &[u32],
    subtree_limit: usize,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<Contraction> {
    let edge_count = edge_rank.len();
    if edge_count > MAX_EDGE_STATES {
        bail!("{edge_count} edge states exceed the {MAX_EDGE_STATES} a hierarchy addresses");
    }
    let base = BaseArcs::new(topology, edge_rank)?;
    let schedule = Schedule::new(&base, edge_order, subtree_limit);
    let pending = (0..edge_count)
        .map(|_| Mutex::new(Vec::new()))
        .collect::<Vec<PendingRow>>();
    let contractor = Contractor {
        base: &base,
        pending: &pending,
        edge_rank,
        is_top: &schedule.is_top,
    };

    // Contracted rows as `[state, out_count, in_count, out.., in..]`
    // records, outgoing heads and incoming tails each sorted by id.
    let mut records = Vec::new();
    let workers = (0..schedule.subtrees.len())
        .into_par_iter()
        .fold(Worker::default, |mut worker, subtree| {
            for &state in schedule.subtrees.group(subtree) {
                contractor.contract(state, &mut worker, Phase::Subtrees);
            }
            worker
        })
        .collect::<Vec<_>>();
    contractor.finish_phase(workers, &mut records);

    let mut contracted = schedule.subtrees.states.len();
    let mut reporter = PercentReporter::starting_at_zero();
    for height in 0..schedule.top.len() {
        let states = schedule.top.group(height);
        let workers = states
            .par_iter()
            .fold(Worker::default, |mut worker, &state| {
                contractor.contract(state, &mut worker, Phase::Top);
                worker
            })
            .collect::<Vec<_>>();
        contractor.finish_phase(workers, &mut records);
        contracted += states.len();
        reporter.emit_if_needed(
            contracted as u64,
            edge_count as u64,
            DatasetImportStage::BuildAcceleration,
            progress,
            |percent| format!("Building CCH {percent:.0}% ({contracted}/{edge_count} edge states)"),
        );
    }
    let base_arc_count = base.keys.len() as u64;
    drop(pending);
    drop(schedule);
    drop(base);

    let rows = ContractedRows::new(&records, edge_count)?;
    let (upward_first_out, upward_head) = rows.upward(&records)?;
    let (downward_first_out, downward_head) = rows.downward(&records)?;
    let total_arc_count = upward_head.len() as u64 + downward_head.len() as u64;
    Ok(Contraction {
        base_arc_count,
        shortcut_arc_count: total_arc_count - base_arc_count,
        upward_first_out,
        upward_head,
        downward_first_out,
        downward_head,
    })
}

/// The distinct base arcs (edge transitions) grouped by owner, each group
/// sorted by key.
struct BaseArcs {
    first: Vec<u32>,
    keys: Vec<u32>,
}

impl BaseArcs {
    fn new(topology: &TopologyBundle, edge_rank: &[u32]) -> Result<Self> {
        let edge_count = edge_rank.len();
        let transitions = &topology.edge_based_topology;
        // `(owner << 32) | key`, so that sorting groups arcs by owner.
        let mut owned = Vec::new();
        if transitions.edge_transition_first_out.len() == edge_count + 1 {
            owned.reserve(transitions.edge_transition_edges.len());
            for edge in 0..edge_count {
                let start = transitions.edge_transition_first_out[edge] as usize;
                let end = transitions.edge_transition_first_out[edge + 1] as usize;
                for &next in &transitions.edge_transition_edges[start..end] {
                    if next as usize == edge || next as usize >= edge_count {
                        continue;
                    }
                    let edge = edge as u32;
                    let (owner, owned_key) = if edge_rank[edge as usize] < edge_rank[next as usize]
                    {
                        (edge, key(next, true))
                    } else {
                        (next, key(edge, false))
                    };
                    owned.push((u64::from(owner) << 32) | u64::from(owned_key));
                }
            }
        }
        owned.par_sort_unstable();
        owned.dedup();
        checked_u32(owned.len(), "base CCH arcs")?;

        let mut first = vec![0_u32; edge_count + 1];
        for &arc in &owned {
            first[(arc >> 32) as usize + 1] += 1;
        }
        for state in 0..edge_count {
            first[state + 1] += first[state];
        }
        let keys = owned.into_iter().map(|arc| arc as u32).collect();
        Ok(Self { first, keys })
    }

    fn row(&self, state: usize) -> &[u32] {
        &self.keys[self.first[state] as usize..self.first[state + 1] as usize]
    }
}

/// Shared inputs of the contraction.
struct Contractor<'a> {
    base: &'a BaseArcs,
    pending: &'a [PendingRow],
    edge_rank: &'a [u32],
    is_top: &'a [bool],
}

/// The two contraction phases.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// One subtree per task: shortcuts owned by top states wait in the
    /// worker until the phase ends, so that threads never queue on the few
    /// rows they all reach.
    Subtrees,
    /// The top states: shortcuts go straight to their owners' rows, and the
    /// shortcuts of a state with many neighbours are generated in parallel.
    Top,
}

/// Neighbours of a contracted state that outrank it.
struct Neighbours<'a> {
    incoming: &'a [u32],
    incoming_rank: &'a [u32],
    outgoing: &'a [u32],
    outgoing_rank: &'a [u32],
}

impl Neighbours<'_> {
    /// Fills `keys` with the shortcuts leaving the `index`-th incoming tail
    /// that it owns and returns the tail. Shortcut `tail -> head` belongs to
    /// whichever endpoint ranks lower.
    fn shortcuts_from_tail(&self, index: usize, keys: &mut Vec<u32>) -> u32 {
        let (tail, tail_rank) = (self.incoming[index], self.incoming_rank[index]);
        keys.clear();
        keys.extend(
            self.outgoing
                .iter()
                .zip(self.outgoing_rank)
                .filter(|&(&head, &head_rank)| head != tail && head_rank > tail_rank)
                .map(|(&head, _)| key(head, true)),
        );
        tail
    }

    /// Fills `keys` with the shortcuts entering the `index`-th outgoing head
    /// that it owns and returns the head.
    fn shortcuts_to_head(&self, index: usize, keys: &mut Vec<u32>) -> u32 {
        let (head, head_rank) = (self.outgoing[index], self.outgoing_rank[index]);
        keys.clear();
        keys.extend(
            self.incoming
                .iter()
                .zip(self.incoming_rank)
                .filter(|&(&tail, &tail_rank)| tail != head && tail_rank > head_rank)
                .map(|(&tail, _)| key(tail, false)),
        );
        head
    }
}

/// Contraction state of one thread.
#[derive(Default)]
struct Worker {
    owned: Vec<u32>,
    incoming: Vec<u32>,
    incoming_rank: Vec<u32>,
    outgoing: Vec<u32>,
    outgoing_rank: Vec<u32>,
    keys: Vec<u32>,
    /// Records of the contracted states.
    record: Vec<u32>,
    /// `(owner << 32) | key` shortcuts owned by top states, handed over
    /// at the end of [`Phase::Subtrees`].
    deferred: Vec<u64>,
}

impl Contractor<'_> {
    /// Contracts `state`: appends its final row to the worker's record and
    /// hands every shortcut it implies to the row of the shortcut's lower
    /// endpoint.
    fn contract(&self, state: u32, worker: &mut Worker, phase: Phase) {
        let Worker {
            owned,
            incoming,
            incoming_rank,
            outgoing,
            outgoing_rank,
            keys,
            record,
            deferred,
        } = worker;
        owned.clear();
        owned.extend_from_slice(self.base.row(state as usize));
        owned.append(&mut std::mem::take(&mut *lock(
            &self.pending[state as usize],
        )));
        owned.sort_unstable();
        owned.dedup();
        incoming.clear();
        outgoing.clear();
        for &owned_key in owned.iter() {
            let other = owned_key >> 1;
            if owned_key & 1 == 1 {
                outgoing.push(other);
            } else {
                incoming.push(other);
            }
        }
        record.extend([state, outgoing.len() as u32, incoming.len() as u32]);
        record.extend_from_slice(outgoing);
        record.extend_from_slice(incoming);

        incoming_rank.clear();
        incoming_rank.extend(incoming.iter().map(|&tail| self.edge_rank[tail as usize]));
        outgoing_rank.clear();
        outgoing_rank.extend(outgoing.iter().map(|&head| self.edge_rank[head as usize]));
        let neighbours = Neighbours {
            incoming,
            incoming_rank,
            outgoing,
            outgoing_rank,
        };
        if phase == Phase::Subtrees {
            for index in 0..neighbours.incoming.len() {
                let owner = neighbours.shortcuts_from_tail(index, keys);
                self.hand_over(owner, keys, Some(&mut *deferred));
            }
            for index in 0..neighbours.outgoing.len() {
                let owner = neighbours.shortcuts_to_head(index, keys);
                self.hand_over(owner, keys, Some(&mut *deferred));
            }
        } else if neighbours.incoming.len() * neighbours.outgoing.len() < PARALLEL_PAIRS {
            for index in 0..neighbours.incoming.len() {
                let owner = neighbours.shortcuts_from_tail(index, keys);
                self.hand_over(owner, keys, None);
            }
            for index in 0..neighbours.outgoing.len() {
                let owner = neighbours.shortcuts_to_head(index, keys);
                self.hand_over(owner, keys, None);
            }
        } else {
            (0..neighbours.incoming.len())
                .into_par_iter()
                .for_each_init(Vec::new, |keys, index| {
                    let owner = neighbours.shortcuts_from_tail(index, keys);
                    self.hand_over(owner, keys, None);
                });
            (0..neighbours.outgoing.len())
                .into_par_iter()
                .for_each_init(Vec::new, |keys, index| {
                    let owner = neighbours.shortcuts_to_head(index, keys);
                    self.hand_over(owner, keys, None);
                });
        }
    }

    /// Gives `keys` to `owner`'s row, or to `deferred` when there is one and
    /// the owner is a top state.
    fn hand_over(&self, owner: u32, keys: &[u32], deferred: Option<&mut Vec<u64>>) {
        if keys.is_empty() {
            return;
        }
        if let Some(deferred) = deferred.filter(|_| self.is_top[owner as usize]) {
            deferred.extend(
                keys.iter()
                    .map(|&owned_key| (u64::from(owner) << 32) | u64::from(owned_key)),
            );
        } else {
            append(&self.pending[owner as usize], keys);
        }
    }

    /// Moves the workers' records to `records` and their deferred shortcuts
    /// to the owners' rows.
    fn finish_phase(&self, workers: Vec<Worker>, records: &mut Vec<Vec<u32>>) {
        let mut deferred = Vec::new();
        for mut worker in workers {
            worker.record.shrink_to_fit();
            records.push(worker.record);
            deferred.append(&mut worker.deferred);
        }
        deferred.par_sort_unstable();
        deferred.dedup();
        deferred
            .par_chunk_by(|left, right| left >> 32 == right >> 32)
            .for_each(|group| {
                let mut row = lock(&self.pending[(group[0] >> 32) as usize]);
                row.extend(group.iter().map(|&arc| arc as u32));
            });
    }
}

/// States split into groups, each listed in rank order.
struct Groups {
    states: Vec<u32>,
    first: Vec<usize>,
}

impl Groups {
    /// Groups the states by `group` id ([`NO_STATE`] for none).
    fn new(edge_order: &[u32], group: &[u32], group_count: usize) -> Self {
        let mut first = vec![0_usize; group_count + 1];
        for &id in group {
            if id != NO_STATE {
                first[id as usize + 1] += 1;
            }
        }
        for id in 0..group_count {
            first[id + 1] += first[id];
        }
        let mut cursor = first[..group_count].to_vec();
        let mut states = vec![0_u32; first[group_count]];
        for &state in edge_order {
            let id = group[state as usize];
            if id != NO_STATE {
                states[cursor[id as usize]] = state;
                cursor[id as usize] += 1;
            }
        }
        Self { states, first }
    }

    fn len(&self) -> usize {
        self.first.len() - 1
    }

    fn group(&self, id: usize) -> &[u32] {
        &self.states[self.first[id]..self.first[id + 1]]
    }
}

/// Contraction schedule from the elimination tree of the undirected base
/// graph: subtrees small enough for one task each, and the remaining top
/// states by height.
struct Schedule {
    subtrees: Groups,
    top: Groups,
    is_top: Vec<bool>,
}

impl Schedule {
    fn new(base: &BaseArcs, edge_order: &[u32], limit: usize) -> Self {
        let edge_count = edge_order.len();
        let parent = elimination_tree(base, edge_order);

        // Children rank below their parent, so rank order settles every
        // subtree size and height before it is propagated.
        let mut size = vec![1_u32; edge_count];
        let mut height = vec![0_u32; edge_count];
        for &state in edge_order {
            let up = parent[state as usize];
            if up != NO_STATE {
                size[up as usize] += size[state as usize];
                height[up as usize] = height[up as usize].max(height[state as usize] + 1);
            }
        }

        let mut subtree = vec![NO_STATE; edge_count];
        let mut subtree_count = 0_u32;
        for &state in edge_order.iter().rev() {
            let state = state as usize;
            if size[state] as usize > limit {
                continue;
            }
            let up = parent[state];
            subtree[state] = if up != NO_STATE && subtree[up as usize] != NO_STATE {
                subtree[up as usize]
            } else {
                subtree_count += 1;
                subtree_count - 1
            };
        }
        drop((parent, size));
        let subtrees = Groups::new(edge_order, &subtree, subtree_count as usize);

        let is_top = subtree.iter().map(|&id| id == NO_STATE).collect::<Vec<_>>();
        let mut top_height = height;
        for (state_height, &top) in top_height.iter_mut().zip(&is_top) {
            if !top {
                *state_height = NO_STATE;
            }
        }
        let height_count = top_height
            .iter()
            .filter(|&&state_height| state_height != NO_STATE)
            .max()
            .map_or(0, |&highest| highest as usize + 1);
        let top = Groups::new(edge_order, &top_height, height_count);
        Self {
            subtrees,
            top,
            is_top,
        }
    }
}

/// Elimination-tree parent of every state in the undirected base graph, by
/// Liu's algorithm: path-compressed ancestors find the root of every lower
/// neighbour's current subtree.
fn elimination_tree(base: &BaseArcs, edge_order: &[u32]) -> Vec<u32> {
    let edge_count = edge_order.len();
    // Lower-ranked base neighbours of every state; a base row lists the
    // higher-ranked ones.
    let mut lower_first = vec![0_u32; edge_count + 1];
    for &owned_key in &base.keys {
        lower_first[(owned_key >> 1) as usize + 1] += 1;
    }
    for state in 0..edge_count {
        lower_first[state + 1] += lower_first[state];
    }
    let mut cursor = lower_first[..edge_count].to_vec();
    let mut lower = vec![0_u32; base.keys.len()];
    for owner in 0..edge_count {
        for &owned_key in base.row(owner) {
            let other = (owned_key >> 1) as usize;
            lower[cursor[other] as usize] = owner as u32;
            cursor[other] += 1;
        }
    }
    drop(cursor);

    let mut parent = vec![NO_STATE; edge_count];
    let mut ancestor = vec![NO_STATE; edge_count];
    for &state in edge_order {
        let range = lower_first[state as usize] as usize..lower_first[state as usize + 1] as usize;
        for &neighbour in &lower[range] {
            let mut walk = neighbour as usize;
            loop {
                let next = ancestor[walk];
                if next == state {
                    break;
                }
                ancestor[walk] = state;
                if next == NO_STATE {
                    parent[walk] = state;
                    break;
                }
                walk = next as usize;
            }
        }
    }
    parent
}

/// Where the contracted row of every state lies in the records.
struct ContractedRows {
    /// `(record index, offset)` of each state's record entry.
    location: Vec<(u32, u32)>,
}

impl ContractedRows {
    fn new(records: &[Vec<u32>], edge_count: usize) -> Result<Self> {
        checked_u32(records.len(), "contraction records")?;
        let mut location = vec![(u32::MAX, 0_u32); edge_count];
        for (index, record) in records.iter().enumerate() {
            checked_u32(record.len(), "contraction record entries")?;
            let mut offset = 0;
            while offset < record.len() {
                let state = record[offset] as usize;
                location[state] = (index as u32, offset as u32);
                offset += 3 + record[offset + 1] as usize + record[offset + 2] as usize;
            }
        }
        debug_assert!(
            location.iter().all(|&(index, _)| index != u32::MAX),
            "every state is contracted once"
        );
        Ok(Self { location })
    }

    /// Outgoing heads and incoming tails of `state`, each sorted by id.
    fn row<'a>(&self, records: &'a [Vec<u32>], state: usize) -> (&'a [u32], &'a [u32]) {
        let (index, offset) = self.location[state];
        let record = &records[index as usize];
        let start = offset as usize + 3;
        let outgoing = record[start - 2] as usize;
        let incoming = record[start - 1] as usize;
        (
            &record[start..start + outgoing],
            &record[start + outgoing..start + outgoing + incoming],
        )
    }

    /// Upward rows: every state's arcs to higher-ranked heads.
    fn upward(&self, records: &[Vec<u32>]) -> Result<(Vec<u32>, Vec<u32>)> {
        let edge_count = self.location.len();
        let mut first_out = Vec::with_capacity(edge_count + 1);
        first_out.push(0_u32);
        let mut total = 0_usize;
        for state in 0..edge_count {
            total += self.row(records, state).0.len();
            first_out.push(checked_u32(total, "upward CCH arcs")?);
        }
        let mut heads = Vec::with_capacity(total);
        for state in 0..edge_count {
            heads.extend_from_slice(self.row(records, state).0);
        }
        Ok((first_out, heads))
    }

    /// Downward rows: every state's arcs to lower-ranked heads, which are
    /// the lower states that list it as an incoming tail.
    fn downward(&self, records: &[Vec<u32>]) -> Result<(Vec<u32>, Vec<u32>)> {
        let edge_count = self.location.len();
        let mut first_out = vec![0_u32; edge_count + 1];
        let mut total = 0_usize;
        for state in 0..edge_count {
            let incoming = self.row(records, state).1;
            total += incoming.len();
            for &tail in incoming {
                first_out[tail as usize + 1] += 1;
            }
        }
        checked_u32(total, "downward CCH arcs")?;
        for state in 0..edge_count {
            first_out[state + 1] += first_out[state];
        }
        // Lower endpoints are visited in id order, so rows come out sorted.
        let mut cursor = first_out[..edge_count].to_vec();
        let mut heads = vec![0_u32; total];
        for state in 0..edge_count {
            for &tail in self.row(records, state).1 {
                let slot = &mut cursor[tail as usize];
                heads[*slot as usize] = state as u32;
                *slot += 1;
            }
        }
        Ok((first_out, heads))
    }
}
