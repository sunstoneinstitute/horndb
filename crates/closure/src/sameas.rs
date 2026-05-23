//! `owl:sameAs` equivalence classes via union-find with path compression and
//! union-by-rank. Hand-rolled (no `union-find` crate dependency — the
//! algorithm is 80 lines and we want full control over the canonical-
//! representative choice).
//!
//! Canonical representative = lexicographically smallest `DictId` in the
//! class (SPEC-05 F4). Because `DictId` is `u64` and dictionary encoding
//! preserves URI ordering for interned URIs (SPEC-02 NF3), the smallest
//! dict ID corresponds to the smallest URI when terms are interned in
//! lexicographic order. Stage 1 accepts this; if the storage layer changes
//! to non-monotonic ID assignment, the canonical-selection rule will need
//! a side table mapping `DictId -> sort key`.

use rustc_hash::FxHashMap;

use crate::types::DictId;

/// Internal slot index in the union-find arrays.
type Slot = u32;
const NIL: Slot = u32::MAX;

#[derive(Default)]
pub struct EquivClasses {
    /// `DictId` -> internal slot index.
    index: FxHashMap<DictId, Slot>,
    /// `slot` -> `DictId` value at that slot.
    values: Vec<DictId>,
    /// Parent slot of each element (self-pointer = root).
    parent: Vec<Slot>,
    /// Rank for union-by-rank (height upper bound).
    rank: Vec<u8>,
    /// For each root slot, the current canonical `DictId` (min of class).
    /// Non-root entries hold a stale value and must not be consulted.
    canon: Vec<DictId>,
}

impl EquivClasses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            index: FxHashMap::with_capacity_and_hasher(cap, Default::default()),
            values: Vec::with_capacity(cap),
            parent: Vec::with_capacity(cap),
            rank: Vec::with_capacity(cap),
            canon: Vec::with_capacity(cap),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Ensure `id` exists as a singleton. Returns its slot.
    pub fn insert(&mut self, id: DictId) -> Slot {
        if let Some(&slot) = self.index.get(&id) {
            return slot;
        }
        let slot = self.values.len() as Slot;
        assert!(slot != NIL, "EquivClasses capacity exhausted (2^32 - 1 entries)");
        self.values.push(id);
        self.parent.push(slot);
        self.rank.push(0);
        self.canon.push(id);
        self.index.insert(id, slot);
        slot
    }

    /// Union the classes containing `a` and `b`. Inserts singletons if needed.
    pub fn union(&mut self, a: DictId, b: DictId) {
        let sa = self.insert(a);
        let sb = self.insert(b);
        let ra = self.find(sa);
        let rb = self.find(sb);
        if ra == rb {
            return;
        }
        let (root, child) = match self.rank[ra as usize].cmp(&self.rank[rb as usize]) {
            std::cmp::Ordering::Less => (rb, ra),
            std::cmp::Ordering::Greater => (ra, rb),
            std::cmp::Ordering::Equal => {
                self.rank[ra as usize] = self.rank[ra as usize].saturating_add(1);
                (ra, rb)
            }
        };
        self.parent[child as usize] = root;
        // Merge canonical: min of the two roots' canonicals.
        let merged = std::cmp::min(self.canon[root as usize], self.canon[child as usize]);
        self.canon[root as usize] = merged;
    }

    /// Find the root of `slot`, with path compression.
    fn find(&mut self, slot: Slot) -> Slot {
        let mut cur = slot;
        while self.parent[cur as usize] != cur {
            // Path halving — single-pass, no recursion.
            let parent = self.parent[cur as usize];
            let grand = self.parent[parent as usize];
            self.parent[cur as usize] = grand;
            cur = grand;
        }
        cur
    }

    /// Returns `true` if `a` and `b` are in the same class.
    pub fn same(&mut self, a: DictId, b: DictId) -> bool {
        let sa = match self.index.get(&a) { Some(&s) => s, None => return false };
        let sb = match self.index.get(&b) { Some(&s) => s, None => return false };
        self.find(sa) == self.find(sb)
    }

    /// Canonical representative of `id`'s class (min DictId in class).
    /// Returns `None` if `id` is unknown.
    pub fn canonical(&self, id: DictId) -> Option<DictId> {
        let slot = *self.index.get(&id)?;
        // Walk to root without compression (immutable receiver).
        let mut cur = slot;
        while self.parent[cur as usize] != cur {
            cur = self.parent[cur as usize];
        }
        Some(self.canon[cur as usize])
    }

    /// Iterate over every member of `id`'s class. O(n) where n = total
    /// elements in the EquivClasses (Stage-1 acceptable; Stage-2 may add
    /// per-root member lists if hot).
    pub fn class_members(&self, id: DictId) -> Box<dyn Iterator<Item = DictId> + '_> {
        let target_canon = match self.canonical(id) {
            Some(c) => c,
            None => return Box::new(std::iter::empty()),
        };
        Box::new(self.values.iter().copied().filter(move |&v| {
            self.canonical(v) == Some(target_canon)
        }))
    }
}
