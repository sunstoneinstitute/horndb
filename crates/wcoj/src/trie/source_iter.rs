//! `PatternTrieIter` — adapts a single `TriplePattern` over a `TripleSource`
//! into a variable-indexed trie iterator.
//!
//! The adapter exposes *local* variable depths `0..arity` corresponding to
//! the variables this pattern mentions, in `var_order` order. The executor
//! maintains the global → local depth mapping per iterator and only calls
//! `peek` / `seek` / `open_level` / `up` for depths the iterator contributes
//! to.

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId};
use crate::pattern::{Term, TriplePattern, Var};
use crate::source::{OrderedTripleIter, TripleSource};
use crate::trie::TrieIterator;

/// Per-physical-depth action: either a literal seek target (`Bound`) or
/// a query-variable position in the *global* `var_order`.
#[derive(Debug, Clone, Copy)]
enum LevelAction {
    Bound(TermId),
    Var(#[allow(dead_code)] u8),
}

pub struct PatternTrieIter<'src> {
    inner: Box<dyn OrderedTripleIter + 'src>,
    /// One entry per physical trie depth (0..3).
    actions: [LevelAction; 3],
    /// Map *local* variable-depth (0..arity) → physical depth.
    var_to_phys: Vec<u8>,
    /// Number of *local* variables — i.e. distinct query variables this
    /// pattern actually mentions.
    arity: u8,
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

        // Mark bound levels first.
        let mut actions = [LevelAction::Bound(0); 3];
        for (phys_d, term) in phys_terms.iter().enumerate() {
            if let Term::Bound(v) = term {
                actions[phys_d] = LevelAction::Bound(*v);
            }
        }

        // Build local var-depth → physical-depth map. Local depth assignment
        // mirrors the global var_order: vars appear in the same relative
        // order as they do in `var_order`.
        let mut var_to_phys: Vec<u8> = Vec::new();
        for var in var_order {
            for (phys_d, term) in phys_terms.iter().enumerate() {
                if *term == Term::Var(*var) {
                    actions[phys_d] = LevelAction::Var(var_to_phys.len() as u8);
                    var_to_phys.push(phys_d as u8);
                    break;
                }
            }
        }

        let arity = var_to_phys.len() as u8;

        // Build the inner iterator and seek/open through any leading bound
        // physical levels until we hit a variable level.
        let mut inner = source.iter(ordering)?;
        for phys_d in 0..3u8 {
            match actions[phys_d as usize] {
                LevelAction::Bound(v) => {
                    inner.seek(phys_d, v);
                    if inner.peek(phys_d) != Some(v) {
                        // No matching row at this level — iterator is empty.
                        return Ok(Self {
                            inner,
                            actions,
                            var_to_phys,
                            arity,
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
            var_to_phys,
            arity,
        })
    }

    fn phys_for(&self, local_depth: u8) -> u8 {
        self.var_to_phys[local_depth as usize]
    }

    /// Reset the inner cursor to the post-construction state: seek through
    /// any leading bound physical levels and `open_level` so the iter is
    /// ready for `peek(0)` at the first variable.
    pub fn reset(&mut self) {
        // Tear down all physical levels first.
        for p in (0..3u8).rev() {
            self.inner.up(p);
        }
        // Re-apply leading bound seeks.
        for phys_d in 0..3u8 {
            match self.actions[phys_d as usize] {
                LevelAction::Bound(v) => {
                    self.inner.seek(phys_d, v);
                    if self.inner.peek(phys_d) != Some(v) {
                        return;
                    }
                    if phys_d < 2 {
                        self.inner.open_level(phys_d + 1);
                    }
                }
                LevelAction::Var(_) => break,
            }
        }
    }
}

impl<'src> TrieIterator for PatternTrieIter<'src> {
    fn arity(&self) -> u8 {
        self.arity
    }

    fn reset(&mut self) {
        PatternTrieIter::reset(self)
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
        // Descend from local var-depth `depth` to local var-depth `depth+1`.
        // In physical terms: open all phys levels between var_to_phys[depth]+1
        // and var_to_phys[depth+1] (inclusive). Bound physical levels in
        // between are seeked & verified.
        let phys_cur = self.phys_for(depth);
        let phys_next = self
            .var_to_phys
            .get((depth + 1) as usize)
            .copied()
            .unwrap_or(3);
        let mut p = phys_cur + 1;
        while p <= phys_next && p < 3 {
            self.inner.open_level(p);
            if p < phys_next {
                if let LevelAction::Bound(v) = self.actions[p as usize] {
                    self.inner.seek(p, v);
                    if self.inner.peek(p) != Some(v) {
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
        // Inverse of open_level(depth-1): undo physical levels touched
        // between phys_for(depth-1)+1 and phys_for(depth).
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
