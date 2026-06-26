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
        let [a, b, c] = ord.permute(self.s, self.p, self.o);
        (a, b, c)
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

    /// Permute three components `(s, p, o)` into this ordering's
    /// `(level0, level1, level2)` layout. The single source of truth for the
    /// six SPO permutations used across the crate.
    #[inline]
    pub fn permute<T>(self, s: T, p: T, o: T) -> [T; 3] {
        match self {
            Ordering::Spo => [s, p, o],
            Ordering::Sop => [s, o, p],
            Ordering::Pso => [p, s, o],
            Ordering::Pos => [p, o, s],
            Ordering::Osp => [o, s, p],
            Ordering::Ops => [o, p, s],
        }
    }
}
