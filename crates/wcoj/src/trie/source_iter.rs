//! `PatternTrieIter` — adapts a single `TriplePattern` over a `TripleSource`
//! into a variable-indexed trie iterator.

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId};
use crate::pattern::{Term, TriplePattern, Var};
use crate::source::{OrderedTripleIter, TripleSource};
use crate::trie::TrieIterator;

/// Per-physical-depth action: either "the level is bound to `TermId` — seek
/// to it and don't expose it as a variable" or "the level corresponds to
/// query variable at slot `usize`".
#[derive(Debug, Clone, Copy)]
enum LevelAction {
    Bound(TermId),
    Var(u8), // Index into the query-variable ordering
}

pub struct PatternTrieIter<'src> {
    inner: Box<dyn OrderedTripleIter + 'src>,
    /// One entry per physical trie depth (0..3). Tells the adapter how to
    /// translate variable-level peek/seek/open into physical operations.
    actions: [LevelAction; 3],
    /// Number of variable levels this pattern contributes (≤ 3).
    arity: u8,
    /// Map variable-depth → physical depth (i.e. the physical depth at which
    /// that variable lives).
    var_to_phys: Vec<u8>,
}

impl<'src> PatternTrieIter<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        pattern: &TriplePattern,
        var_order: &[Var],
        ordering: Ordering,
    ) -> Result<Self> {
        if !source.supports(ordering) {
            return Err(WcojError::OrderingUnavailable(ordering));
        }

        // Compute the three physical-level Terms in trie order.
        let phys_terms = match ordering {
            Ordering::Spo => [pattern.s, pattern.p, pattern.o],
            Ordering::Sop => [pattern.s, pattern.o, pattern.p],
            Ordering::Pso => [pattern.p, pattern.s, pattern.o],
            Ordering::Pos => [pattern.p, pattern.o, pattern.s],
            Ordering::Osp => [pattern.o, pattern.s, pattern.p],
            Ordering::Ops => [pattern.o, pattern.p, pattern.s],
        };

        // Build LevelActions and the variable-depth → physical-depth map.
        // Variables in the pattern that appear in `var_order` are exposed at
        // depths corresponding to their position in `var_order` for *this*
        // pattern's contribution.
        let mut actions = [LevelAction::Bound(0); 3];
        let mut var_to_phys = Vec::new();

        // First, mark bound levels.
        for (phys_d, term) in phys_terms.iter().enumerate() {
            if let Term::Bound(v) = term {
                actions[phys_d] = LevelAction::Bound(*v);
            }
        }

        // Then, walk `var_order` and assign each var that's in this pattern
        // to its physical depth, in the order it appears in `var_order`.
        for (var_slot, var) in var_order.iter().enumerate() {
            for (phys_d, term) in phys_terms.iter().enumerate() {
                if *term == Term::Var(*var) {
                    actions[phys_d] = LevelAction::Var(var_slot as u8);
                    var_to_phys.push(phys_d as u8);
                    break;
                }
            }
        }

        let arity = var_to_phys.len() as u8;

        // Build inner iterator and immediately seek through any bound prefix
        // *until* we hit a variable level. Bound levels deeper than the first
        // variable are applied lazily inside `open_level` / `peek`.
        let mut inner = source.iter(ordering)?;
        // Position the cursor through any leading bound levels.
        for phys_d in 0..3u8 {
            match actions[phys_d as usize] {
                LevelAction::Bound(v) => {
                    inner.seek(phys_d, v);
                    if inner.peek(phys_d) != Some(v) {
                        // No matching row — the iterator is empty.
                        return Ok(Self {
                            inner,
                            actions,
                            arity,
                            var_to_phys,
                        });
                    }
                    if phys_d < 2 {
                        inner.open_level(phys_d + 1);
                    }
                }
                LevelAction::Var(_) => break,
            }
        }

        Ok(Self {
            inner,
            actions,
            arity,
            var_to_phys,
        })
    }

    /// Resolve a variable-depth to the underlying physical depth, applying any
    /// trailing bound levels in between.
    fn phys_for(&self, var_depth: u8) -> u8 {
        self.var_to_phys[var_depth as usize]
    }
}

impl<'src> TrieIterator for PatternTrieIter<'src> {
    fn arity(&self) -> u8 {
        self.arity
    }

    fn peek(&self, depth: u8) -> Option<TermId> {
        let phys = self.phys_for(depth);
        self.inner.peek(phys)
    }

    fn seek(&mut self, depth: u8, value: TermId) {
        let phys = self.phys_for(depth);
        self.inner.seek(phys, value);
    }

    fn open_level(&mut self, depth: u8) {
        // TrieIterator semantics: we just chose a value at var-depth `depth`
        // (callable via peek(depth)); descend so that peek(depth+1) works.
        // In physical-depth terms: we're positioned at `phys_cur =
        // var_to_phys[depth]` and want to expose values at `phys_next =
        // var_to_phys[depth+1]` (or do nothing if this was the last var).
        let phys_cur = self.phys_for(depth);
        let phys_next = self
            .var_to_phys
            .get((depth + 1) as usize)
            .copied()
            .unwrap_or(3);
        // Walk physical depths phys_cur+1 ..= phys_next, calling open_level
        // on each. For intermediate bound levels (between this variable and
        // the next), also seek to the bound value and check it matches.
        let mut p = phys_cur + 1;
        while p <= phys_next && p < 3 {
            self.inner.open_level(p);
            if p < phys_next {
                if let LevelAction::Bound(v) = self.actions[p as usize] {
                    self.inner.seek(p, v);
                    if self.inner.peek(p) != Some(v) {
                        // Mark exhausted: force peek at the next variable
                        // level to return None by seeking past max.
                        if phys_next < 3 {
                            self.inner.seek(phys_next, TermId::MAX);
                        }
                        return;
                    }
                }
            }
            p += 1;
        }
    }

    fn up(&mut self, depth: u8) {
        // Inverse of open_level: undo the descent from var-depth `depth-1`
        // into var-depth `depth`. Physical levels touched are
        // phys[depth-1]+1 ..= phys[depth] (plus any trailing bounds for the
        // last variable).
        let phys_cur = self.phys_for(depth);
        let phys_parent_excl = if depth == 0 {
            0u8
        } else {
            self.phys_for(depth - 1) + 1
        };
        let mut p = phys_parent_excl;
        while p <= phys_cur {
            self.inner.up(p);
            p += 1;
        }
    }
}
