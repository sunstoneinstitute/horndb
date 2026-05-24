//! Leapfrog Triejoin executor.
//!
//! Drives a depth-first leapfrog intersection over a set of pattern trie
//! iterators. Each iterator exposes *local* variable depths (the variables
//! it mentions, in the same relative order as the executor's global
//! `var_order`); the executor maintains a per-iterator local-depth cursor
//! that advances/retreats as we descend and ascend.
//!
//! The recursion is implemented with an explicit per-depth state stack so
//! cancellation polling stays cheap and the hot path has no recursion
//! frames.

use std::sync::Arc;

use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::Result;
use crate::ids::TermId;
use crate::pattern::Bgp;
use crate::plan::ExecutionPlan;
use crate::source::TripleSource;
use crate::trie::leapfrog::LeapfrogJoin;
use crate::trie::source_iter::PatternTrieIter;
use crate::trie::TrieIterator;

pub struct WcojExecutor<'src> {
    source: &'src dyn TripleSource,
    bgp: Arc<Bgp>,
    plan: Arc<ExecutionPlan>,
    cancel: CancelToken,
}

impl<'src> WcojExecutor<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        plan: &ExecutionPlan,
        cancel: CancelToken,
    ) -> Self {
        Self {
            source,
            bgp: Arc::new(bgp.clone()),
            plan: Arc::new(plan.clone()),
            cancel,
        }
    }

    pub fn into_iter(self) -> BatchIter<'src> {
        BatchIter::new(self)
    }
}

/// Wrap a `PatternTrieIter` so the leapfrog can address it at the *current
/// global depth* even when that global depth maps to a local depth specific
/// to the iter. The wrapper translates global-depth peek/seek/open/up calls
/// into the iter's local-depth calls.
///
/// The translation table `g_to_l[g] = Some(local_d)` is set up at executor
/// init; `cur_local_depth` tracks where the inner iter's cursor currently
/// sits.
struct AdaptiveIter<'src> {
    inner: PatternTrieIter<'src>,
    /// `g_to_l[global_depth]` = Some(local_depth) if this pattern mentions
    /// the variable at that global depth.
    g_to_l: Vec<Option<u8>>,
}

impl<'src> AdaptiveIter<'src> {
    fn local_for(&self, global_depth: u8) -> u8 {
        self.g_to_l[global_depth as usize]
            .expect("AdaptiveIter queried at a non-contributing global depth")
    }
}

impl<'src> TrieIterator for AdaptiveIter<'src> {
    fn arity(&self) -> u8 {
        self.inner.arity()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn peek(&self, depth: u8) -> Option<TermId> {
        self.inner.peek(self.local_for(depth))
    }
    fn seek(&mut self, depth: u8, value: TermId) {
        let local = self.local_for(depth);
        self.inner.seek(local, value);
    }
    /// `open_level(global_depth)` ⇒ `inner.open_level(local_for(global_depth))`.
    /// This descends past the iter's local var at `global_depth` to expose
    /// the next contribution (if any).
    fn open_level(&mut self, depth: u8) {
        let local = self.local_for(depth);
        self.inner.open_level(local);
    }
    /// `up(global_depth)` is called by the executor when ascending past
    /// global depth (back to the parent at depth-1). We undo the matching
    /// `open_level(global_depth - 1)` call.
    fn up(&mut self, depth: u8) {
        if depth == 0 {
            return;
        }
        // Find the most recent global depth strictly less than `depth` that
        // this iter contributed to (and thus called open_level on).
        let mut prev_local: Option<u8> = None;
        for g in (0..depth as usize).rev() {
            if let Some(l) = self.g_to_l[g] {
                prev_local = Some(l);
                break;
            }
        }
        if let Some(l) = prev_local {
            // open_level(l) exposed local depth l+1 (and any trailing
            // bound levels). Undo via inner.up(l+1).
            self.inner.up(l + 1);
        }
        // If no previous contribution exists, the iter never descended for
        // this branch — nothing to undo.
    }
}

/// Output iterator: drives the leapfrog loop, flushes Arrow batches.
pub struct BatchIter<'src> {
    exec: WcojExecutor<'src>,
    builder: BindingBatchBuilder,
    /// All adaptive iterators. When a join at depth d is active, the
    /// contributing iters are *moved* into that join (replaced here by
    /// a sentinel); they're returned on ascent.
    iters: Vec<Box<dyn TrieIterator + 'src>>,
    /// For each variable depth: indices of `iters` that mention this var.
    contributing: Vec<Vec<usize>>,
    /// For each variable depth `d`: indices of iters that mention `d` AND
    /// also mention some deeper variable. These are the iters that need
    /// `open_level(d)` / `up(d+1)` when descending/ascending past `d`.
    descend_at: Vec<Vec<usize>>,
    /// Per-depth join state.
    join_state: Vec<Option<LeapfrogJoin<'src>>>,
    /// Current binding values per depth.
    binding: Vec<TermId>,
    /// Current recursion depth (== global variable index being processed).
    depth: u8,
    finished: bool,
    pending_error: Option<crate::error::WcojError>,
}

impl<'src> BatchIter<'src> {
    fn new(exec: WcojExecutor<'src>) -> Self {
        let n_vars = exec.plan.var_order.len();
        let builder = BindingBatchBuilder::new(exec.plan.var_order.clone());
        let mut iters: Vec<Box<dyn TrieIterator + 'src>> = Vec::new();
        let mut pending_error = None;

        // Build one AdaptiveIter per pattern.
        for pat in &exec.bgp.patterns {
            match PatternTrieIter::new(
                exec.source,
                pat,
                &exec.plan.var_order,
                pat.ordering_for(&exec.plan.var_order),
            ) {
                Ok(inner) => {
                    // Build the global → local map.
                    let mut g_to_l: Vec<Option<u8>> = vec![None; n_vars];
                    let mut local = 0u8;
                    for (g, var) in exec.plan.var_order.iter().enumerate() {
                        if pat.position_of(*var).is_some() {
                            g_to_l[g] = Some(local);
                            local += 1;
                        }
                    }
                    iters.push(Box::new(AdaptiveIter { inner, g_to_l }));
                }
                Err(e) => {
                    pending_error = Some(e);
                    break;
                }
            }
        }

        // Compute, for each variable depth, which patterns contribute.
        let mut contributing = Vec::with_capacity(n_vars);
        for var in &exec.plan.var_order {
            let mut v = Vec::new();
            for (i, pat) in exec.bgp.patterns.iter().enumerate() {
                if pat.position_of(*var).is_some() {
                    v.push(i);
                }
            }
            contributing.push(v);
        }
        // Compute, for each depth, which contributing iters also mention a
        // deeper variable (and thus need open_level/up on descent/ascent).
        let mut descend_at: Vec<Vec<usize>> = Vec::with_capacity(n_vars);
        for d in 0..n_vars {
            let mut v = Vec::new();
            for &i in &contributing[d] {
                let pat = &exec.bgp.patterns[i];
                let has_deeper = exec.plan.var_order[(d + 1)..]
                    .iter()
                    .any(|var| pat.position_of(*var).is_some());
                if has_deeper {
                    v.push(i);
                }
            }
            descend_at.push(v);
        }

        let join_state = (0..n_vars).map(|_| None).collect();
        let binding = vec![0; n_vars];

        Self {
            exec,
            builder,
            iters,
            contributing,
            descend_at,
            join_state,
            binding,
            depth: 0,
            finished: false,
            pending_error,
        }
    }

    fn step(&mut self) -> Option<Result<RecordBatch>> {
        if let Some(e) = self.pending_error.take() {
            self.finished = true;
            return Some(Err(e));
        }
        if self.finished {
            return None;
        }

        let n_vars = self.exec.plan.var_order.len() as u8;

        if n_vars == 0 {
            self.finished = true;
            return None;
        }

        loop {
            if self.depth == 0 {
                if let Err(e) = self.exec.cancel.check() {
                    self.finished = true;
                    return Some(Err(e));
                }
            }

            // Initialise the join at this depth if needed.
            let needs_init = match &mut self.join_state[self.depth as usize] {
                None => true,
                Some(j) => j.iters_mut().is_empty(),
            };

            if needs_init {
                let idxs = self.contributing[self.depth as usize].clone();
                let mut taken: Vec<Box<dyn TrieIterator + 'src>> =
                    Vec::with_capacity(idxs.len());
                for &i in &idxs {
                    let placeholder: Box<dyn TrieIterator + 'src> = Box::new(NoopTrieIter);
                    let real = std::mem::replace(&mut self.iters[i], placeholder);
                    taken.push(real);
                }
                self.join_state[self.depth as usize] =
                    Some(LeapfrogJoin::new(taken, self.depth));
            }

            let join = self.join_state[self.depth as usize].as_mut().unwrap();
            let next = join.next();

            match next {
                Some(v) => {
                    self.binding[self.depth as usize] = v;
                    if self.depth + 1 == n_vars {
                        let flushed = self.builder.push_row(&self.binding);
                        if let Some(b) = flushed {
                            return Some(Ok(b));
                        }
                        // Stay at this depth.
                    } else {
                        // Descend: return iters to self.iters, then call
                        // open_level(depth) only on those that have a
                        // deeper variable (descend_at[depth]).
                        let idxs = self.contributing[self.depth as usize].clone();
                        let placeholder = LeapfrogJoin::reentry_marker(self.depth);
                        let taken_join = std::mem::replace(
                            self.join_state[self.depth as usize].as_mut().unwrap(),
                            placeholder,
                        );
                        let returned = taken_join.into_iters();
                        for (i, real) in idxs.iter().zip(returned) {
                            self.iters[*i] = real;
                        }
                        for &i in &self.descend_at[self.depth as usize] {
                            self.iters[i].open_level(self.depth);
                        }
                        self.depth += 1;
                    }
                }
                None => {
                    // Exhausted at this depth.
                    let join = self.join_state[self.depth as usize].take().unwrap();
                    let idxs = self.contributing[self.depth as usize].clone();
                    let returned = join.into_iters();
                    for (i, real) in idxs.iter().zip(returned) {
                        self.iters[*i] = real;
                    }
                    if self.depth == 0 {
                        self.finished = true;
                        if let Some(b) = self.builder.finish() {
                            return Some(Ok(b));
                        }
                        return None;
                    }
                    // Ascend. Before adjusting `self.depth`, note that the
                    // current depth (d+1) just finished iterating; any iters
                    // whose top-contribution depth is exactly d+1 had their
                    // cursor advanced and must be reset so that the next
                    // re-entry sees the full set of values.
                    let finished_depth = self.depth as usize;
                    let to_reset: Vec<usize> = self.contributing[finished_depth]
                        .iter()
                        .copied()
                        .filter(|&i| {
                            // Reset if iter has no contribution at any
                            // depth shallower than `finished_depth`.
                            (0..finished_depth).all(|d| {
                                !self.contributing[d].contains(&i)
                            })
                        })
                        .collect();
                    for i in to_reset {
                        self.iters[i].reset();
                    }
                    self.depth -= 1;
                    // Undo open_level on parent's descending iters
                    // (those that had a deeper var and called open_level).
                    for &i in &self.descend_at[self.depth as usize] {
                        self.iters[i].up(self.depth + 1);
                    }
                    // Advance the parent leapfrog past the value we
                    // descended on. This applies to all contributing iters
                    // at the parent depth.
                    let parent_val = self.binding[self.depth as usize];
                    let parent_idxs = self.contributing[self.depth as usize].clone();
                    for i in &parent_idxs {
                        self.iters[*i].seek(self.depth, parent_val.wrapping_add(1));
                    }
                    // Clear the reentry_marker so init rebuilds the join.
                    self.join_state[self.depth as usize] = None;
                }
            }
        }
    }
}

impl<'src> Iterator for BatchIter<'src> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        self.step()
    }
}

/// Placeholder trie iterator used while real iters are temporarily moved
/// into a `LeapfrogJoin`. Never queried — must be replaced before use.
struct NoopTrieIter;

impl TrieIterator for NoopTrieIter {
    fn arity(&self) -> u8 {
        0
    }
    fn peek(&self, _: u8) -> Option<TermId> {
        None
    }
    fn seek(&mut self, _: u8, _: TermId) {}
    fn open_level(&mut self, _: u8) {}
    fn up(&mut self, _: u8) {}
}
