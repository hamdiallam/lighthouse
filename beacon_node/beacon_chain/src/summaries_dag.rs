use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};
use store::HotStateSummary;
use types::{Hash256, Slot};

#[derive(Debug, Clone, Copy)]
pub struct DAGStateSummary {
    pub slot: Slot,
    pub latest_block_root: Hash256,
    pub previous_state_root: Hash256,
}

#[derive(Debug, Clone, Copy)]
pub struct DAGStateSummaryV22 {
    pub slot: Slot,
    pub latest_block_root: Hash256,
}

pub struct StateSummariesDAG {
    // state_root -> state_summary
    state_summaries_by_state_root: HashMap<Hash256, DAGStateSummary>,
    // block_root -> state slot -> (state_root, state summary)
    state_summaries_by_block_root: HashMap<Hash256, BTreeMap<Slot, (Hash256, DAGStateSummary)>>,
}

#[derive(Debug)]
pub enum Error {
    MissingParentBlockRoot(Hash256),
    MissingStateSummary(Hash256),
    MissingStateSummaryAtSlot(Hash256, Slot),
    MissingChildBlockRoot(Hash256),
    MissingBlock(Hash256),
    RequestedSlotAboveSummary(Hash256, Slot),
}

impl StateSummariesDAG {
    pub fn new(state_summaries: Vec<(Hash256, DAGStateSummary)>) -> Self {
        // Group them by latest block root, and sorted state slot
        let mut state_summaries_by_block_root = HashMap::<_, BTreeMap<_, _>>::new();
        let mut state_summaries_by_state_root = HashMap::new();
        for (state_root, summary) in state_summaries.into_iter() {
            let summaries = state_summaries_by_block_root
                .entry(summary.latest_block_root)
                .or_default();

            // TODO(hdiff): error if existing
            summaries.insert(summary.slot, (state_root, summary));

            state_summaries_by_state_root.insert(state_root, summary);
        }

        Self {
            state_summaries_by_state_root,
            state_summaries_by_block_root,
        }
    }

    pub fn new_from_v22(
        state_summaries_v22: Vec<(Hash256, DAGStateSummaryV22)>,
        parent_block_roots: HashMap<Hash256, Hash256>,
        base_root: Hash256,
    ) -> Result<Self, Error> {
        // Group them by latest block root, and sorted state slot
        let mut state_summaries_by_block_root = HashMap::<_, BTreeMap<_, _>>::new();
        for (state_root, summary) in state_summaries_v22.iter() {
            let summaries = state_summaries_by_block_root
                .entry(summary.latest_block_root)
                .or_default();

            // TODO(hdiff): error if existing
            summaries.insert(summary.slot, (state_root, summary));
        }

        let state_summaries = state_summaries_v22
            .iter()
            .map(|(state_root, summary)| {
                let previous_state_root = if summary.slot == 0 || *state_root == base_root {
                    Hash256::ZERO
                } else {
                    let previous_slot = summary.slot - 1;

                    // Check the set of states in the same state's block root
                    let same_block_root_summaries = state_summaries_by_block_root
                        .get(&summary.latest_block_root)
                        .ok_or(Error::MissingStateSummary(summary.latest_block_root))?;
                    if let Some((state_root, _)) = same_block_root_summaries.get(&previous_slot) {
                        // Skipped slot: block root at previous slot is the same as latest block root.
                        **state_root
                    } else {
                        // Common case: not a skipped slot.
                        let parent_block_root = parent_block_roots
                            .get(&summary.latest_block_root)
                            .ok_or(Error::MissingParentBlockRoot(summary.latest_block_root))?;
                        *state_summaries_by_block_root
                            .get(parent_block_root)
                            .ok_or(Error::MissingStateSummary(*parent_block_root))?
                            .get(&previous_slot)
                            .ok_or(Error::MissingStateSummaryAtSlot(
                                *parent_block_root,
                                previous_slot,
                            ))?
                            .0
                    }
                };

                Ok((
                    *state_root,
                    DAGStateSummary {
                        slot: summary.slot,
                        latest_block_root: summary.latest_block_root,
                        previous_state_root,
                    },
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::new(state_summaries))
    }

    pub fn summaries_count(&self) -> usize {
        self.state_summaries_by_block_root
            .values()
            .map(|s| s.len())
            .sum()
    }

    pub fn summaries_by_slot_ascending(&self) -> BTreeMap<Slot, Vec<(Hash256, DAGStateSummary)>> {
        let mut summaries = BTreeMap::<Slot, Vec<_>>::new();
        for (slot, (state_root, summary)) in self
            .state_summaries_by_block_root
            .values()
            .flat_map(|slot_map| slot_map.iter())
        {
            summaries
                .entry(*slot)
                .or_default()
                .push((*state_root, *summary));
        }
        summaries
    }

    pub fn previous_state_root(&self, state_root: Hash256) -> Result<Hash256, Error> {
        Ok(self
            .state_summaries_by_state_root
            .get(&state_root)
            .ok_or(Error::MissingStateSummary(state_root))?
            .previous_state_root)
    }

    pub fn ancestor_state_root_at_slot(
        &self,
        mut state_root: Hash256,
        ancestor_slot: Slot,
    ) -> Result<Hash256, Error> {
        // Walk backwards until we reach the state at `ancestor_slot`.
        loop {
            let summary = self
                .state_summaries_by_state_root
                .get(&state_root)
                .ok_or(Error::MissingStateSummary(state_root))?;

            // Assumes all summaries are contiguous
            match summary.slot.cmp(&ancestor_slot) {
                Ordering::Less => {
                    return Err(Error::RequestedSlotAboveSummary(state_root, ancestor_slot))
                }
                Ordering::Equal => {
                    return Ok(state_root);
                }
                Ordering::Greater => {
                    state_root = summary.previous_state_root;
                }
            }
        }
    }

    /// Returns all ancestors of `state_root` INCLUDING `state_root` until the next parent is not
    /// known.
    pub fn ancestors_of(&self, mut state_root: Hash256) -> Result<Vec<(Hash256, Slot)>, Error> {
        // Sanity check that the first summary exists
        if !self.state_summaries_by_state_root.contains_key(&state_root) {
            return Err(Error::MissingStateSummary(state_root));
        }

        let mut ancestors = vec![];
        loop {
            if let Some(summary) = self.state_summaries_by_state_root.get(&state_root) {
                ancestors.push((state_root, summary.slot));
                state_root = summary.previous_state_root
            } else {
                return Ok(ancestors);
            }
        }
    }
}

impl From<HotStateSummary> for DAGStateSummaryV22 {
    fn from(value: HotStateSummary) -> Self {
        Self {
            slot: value.slot,
            latest_block_root: value.latest_block_root,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DAGBlockSummary {
    pub slot: Slot,
    pub parent_root: Hash256,
}

pub struct BlockSummariesDAG {
    // parent_block_root -> Vec<children_block_root>
    child_block_roots: HashMap<Hash256, Vec<(Hash256, DAGBlockSummary)>>,
    // block_root -> block
    blocks_by_block_root: HashMap<Hash256, DAGBlockSummary>,
}

impl BlockSummariesDAG {
    pub fn new(blocks: &[(Hash256, DAGBlockSummary)]) -> Self {
        // Construct block root to parent block root mapping.
        let mut child_block_roots = HashMap::<_, Vec<_>>::new();
        let mut blocks_by_block_root = HashMap::new();

        for (block_root, block) in blocks {
            child_block_roots
                .entry(block.parent_root)
                .or_default()
                .push((*block_root, *block));
            // Add empty entry for the child block
            child_block_roots.entry(*block_root).or_default();

            blocks_by_block_root.insert(*block_root, *block);
        }

        Self {
            child_block_roots,
            blocks_by_block_root,
        }
    }

    pub fn descendant_block_roots_of(&self, block_root: &Hash256) -> Result<Vec<Hash256>, Error> {
        let mut descendants = vec![];
        for (child_root, _) in self
            .child_block_roots
            .get(block_root)
            .ok_or(Error::MissingChildBlockRoot(*block_root))?
        {
            descendants.push(*child_root);
            descendants.extend(self.descendant_block_roots_of(child_root)?);
        }
        Ok(descendants)
    }

    /// Returns all ancestors of `block_root` INCLUDING `block_root` until the next parent is not
    /// known.
    pub fn ancestors_of(&self, mut block_root: Hash256) -> Result<Vec<(Hash256, Slot)>, Error> {
        // Sanity check that the first block exists
        if !self.blocks_by_block_root.contains_key(&block_root) {
            return Err(Error::MissingBlock(block_root));
        }

        let mut ancestors = vec![];
        loop {
            if let Some(block) = self.blocks_by_block_root.get(&block_root) {
                ancestors.push((block_root, block.slot));
                block_root = block.parent_root
            } else {
                return Ok(ancestors);
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Hash256, Slot)> + '_ {
        self.blocks_by_block_root
            .iter()
            .map(|(block_root, block)| (*block_root, block.slot))
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockSummariesDAG, DAGBlockSummary, DAGStateSummaryV22, Error, StateSummariesDAG};
    use bls::FixedBytesExtended;
    use std::collections::HashMap;
    use types::{Hash256, Slot};

    fn root(n: u64) -> Hash256 {
        Hash256::from_low_u64_le(n)
    }

    fn block_with_parent(parent_root: Hash256) -> DAGBlockSummary {
        DAGBlockSummary {
            slot: Slot::new(0),
            parent_root,
        }
    }

    #[test]
    fn new_from_v22_empty() {
        StateSummariesDAG::new_from_v22(vec![], HashMap::new(), Hash256::ZERO).unwrap();
    }

    #[test]
    fn new_from_v22_one_state() {
        let root_a = root(0xa);
        let root_1 = root(1);
        let root_2 = root(2);
        let summary_1 = DAGStateSummaryV22 {
            slot: Slot::new(1),
            latest_block_root: root_1,
        };
        //                                 (child, parent)
        let parents = HashMap::from_iter([(root_1, root_2)]);

        let dag =
            StateSummariesDAG::new_from_v22(vec![(root_a, summary_1)], parents, root_a).unwrap();

        // The parent of the root summary is ZERO
        assert_eq!(dag.previous_state_root(root_a).unwrap(), Hash256::ZERO);
    }

    #[test]
    fn descendant_block_roots_of() {
        let root_1 = root(1);
        let root_2 = root(2);
        let root_3 = root(3);
        let parents = vec![(root_1, block_with_parent(root_2))];
        let dag = BlockSummariesDAG::new(&parents);

        // root 1 is known and has no childs
        assert_eq!(
            dag.descendant_block_roots_of(&root_1).unwrap(),
            Vec::<Hash256>::new()
        );
        // root 2 is known and has childs
        assert_eq!(
            dag.descendant_block_roots_of(&root_2).unwrap(),
            vec![root_1]
        );
        // root 3 is not known
        {
            let err = dag.descendant_block_roots_of(&root_3).unwrap_err();
            if let Error::MissingChildBlockRoot(_) = err {
                // ok
            } else {
                panic!("unexpected err {err:?}");
            }
        }
    }
}
