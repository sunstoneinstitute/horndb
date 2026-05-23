/// Internal 64-bit identifier for any RDF term (URI, literal, blank node).
/// SPEC-02 owns the term-kind tagging in the high bits; we treat IDs as opaque.
pub type TermId = u64;

/// A concrete triple in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Triple {
    pub s: TermId,
    pub p: TermId,
    pub o: TermId,
}

impl Triple {
    pub fn new(s: TermId, p: TermId, o: TermId) -> Self {
        Self { s, p, o }
    }

    /// Reorder the triple components according to `ord`, returning a 3-tuple
    /// `(level0, level1, level2)`. Used by the trie iterator to read components
    /// in trie-depth order regardless of physical ordering.
    pub fn by_ordering(&self, ord: Ordering) -> (TermId, TermId, TermId) {
        match ord {
            Ordering::Spo => (self.s, self.p, self.o),
            Ordering::Sop => (self.s, self.o, self.p),
            Ordering::Pso => (self.p, self.s, self.o),
            Ordering::Pos => (self.p, self.o, self.s),
            Ordering::Osp => (self.o, self.s, self.p),
            Ordering::Ops => (self.o, self.p, self.s),
        }
    }
}

/// The six trie orderings. Names follow the convention `<level0><level1><level2>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordering {
    Spo,
    Sop,
    Pso,
    Pos,
    Osp,
    Ops,
}

impl Ordering {
    pub const ALL: [Ordering; 6] = [
        Ordering::Spo,
        Ordering::Sop,
        Ordering::Pso,
        Ordering::Pos,
        Ordering::Osp,
        Ordering::Ops,
    ];
}
