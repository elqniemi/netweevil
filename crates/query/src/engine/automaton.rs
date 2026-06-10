use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct RestrictionAutomaton {
    states: Vec<AutomatonState>,
}

#[derive(Default)]
struct AutomatonState {
    transitions: HashMap<usize, usize>,
    failure: usize,
    prohibited_next: Vec<usize>,
    prohibited_meta: HashMap<usize, usize>,
}

impl RestrictionAutomaton {
    pub(crate) fn build(sequences: &[Vec<usize>]) -> Self {
        let mut automaton = Self {
            states: vec![AutomatonState::default()],
        };

        for sequence in sequences {
            if sequence.len() < 2 {
                continue;
            }
            let mut state_index = 0_usize;
            for &edge_index in sequence.iter().take(sequence.len() - 1) {
                if let Some(&next_state) =
                    automaton.states[state_index].transitions.get(&edge_index)
                {
                    state_index = next_state;
                    continue;
                }
                let next_state = automaton.states.len();
                automaton.states.push(AutomatonState::default());
                automaton.states[state_index]
                    .transitions
                    .insert(edge_index, next_state);
                state_index = next_state;
            }
            automaton.states[state_index]
                .prohibited_next
                .push(*sequence.last().unwrap());
            let next_edge = *sequence.last().unwrap();
            let sequence_len = sequence.len();
            automaton.states[state_index]
                .prohibited_meta
                .entry(next_edge)
                .and_modify(|existing| *existing = (*existing).max(sequence_len))
                .or_insert(sequence_len);
        }

        let mut queue = std::collections::VecDeque::new();
        let root_transitions = automaton.states[0]
            .transitions
            .values()
            .copied()
            .collect::<Vec<_>>();
        for state_index in root_transitions {
            automaton.states[state_index].failure = 0;
            queue.push_back(state_index);
        }

        while let Some(state_index) = queue.pop_front() {
            let transitions = automaton.states[state_index]
                .transitions
                .clone()
                .into_iter()
                .collect::<Vec<_>>();
            for (edge_index, next_state) in transitions {
                let mut failure = automaton.states[state_index].failure;
                while failure != 0
                    && !automaton.states[failure]
                        .transitions
                        .contains_key(&edge_index)
                {
                    failure = automaton.states[failure].failure;
                }
                let failure_state = automaton.states[failure]
                    .transitions
                    .get(&edge_index)
                    .copied()
                    .unwrap_or(0);
                automaton.states[next_state].failure = failure_state;
                let inherited = automaton.states[failure_state].prohibited_next.clone();
                automaton.states[next_state]
                    .prohibited_next
                    .extend(inherited);
                for (&next_edge, &sequence_len) in
                    &automaton.states[failure_state].prohibited_meta.clone()
                {
                    automaton.states[next_state]
                        .prohibited_meta
                        .entry(next_edge)
                        .and_modify(|existing| *existing = (*existing).max(sequence_len))
                        .or_insert(sequence_len);
                }
                automaton.states[next_state].prohibited_next.sort_unstable();
                automaton.states[next_state].prohibited_next.dedup();
                queue.push_back(next_state);
            }
        }

        automaton
    }

    pub(crate) fn is_trivial(&self) -> bool {
        self.states.len() <= 1
    }

    pub(crate) fn transition(&self, state_index: usize, edge_index: usize) -> usize {
        let mut cursor = state_index;
        loop {
            if let Some(&next_state) = self.states[cursor].transitions.get(&edge_index) {
                return next_state;
            }
            if cursor == 0 {
                return 0;
            }
            cursor = self.states[cursor].failure;
        }
    }

    pub(crate) fn is_transition_allowed(&self, state_index: usize, edge_index: usize) -> bool {
        self.states[state_index]
            .prohibited_next
            .binary_search(&edge_index)
            .is_err()
    }

    pub(crate) fn prohibited_sequence_len(
        &self,
        state_index: usize,
        edge_index: usize,
    ) -> Option<usize> {
        self.states[state_index]
            .prohibited_meta
            .get(&edge_index)
            .copied()
    }
}
