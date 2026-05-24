//! Leapfrog Triejoin executor.
//!
//! Drives a depth-first leapfrog intersection over a set of pattern trie
//! iterators. Each iterator exposes *local* variable depths; the executor
//! maintains a per-iterator local-depth cursor that advances/retreats as we
//! descend and ascend.
//!
//! The recursion is implemented with an explicit per-depth state stack so
//! cancellation polling stays cheap. The leapfrog at each depth is inlined
//! against `&mut [Box<dyn TrieIterator>]` rather than owning the iters, to
//! avoid hot-path Box allocations.

use std::sync::Arc;

use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::Result;
use crate::ids::TermId;
use crate::pattern::Bgp;
use crate::plan::ExecutionPlan;
use crate::source::TripleSource;
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
    fn open_level(&mut self, depth: u8) {
        let local = self.local_for(depth);
        self.inner.open_level(local);
    }
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
            self.inner.up(l + 1);
        }
    }
}

/// Per-depth state for the inlined leapfrog. We hold no iter references;
/// the executor borrows iters from `BatchIter::iters` by index list.
struct DepthState {
    /// Round-robin index into the depth's contributing list.
    p: usize,
    /// True before the first call advances iters.
    primed: bool,
    /// True once the leapfrog is exhausted at this depth.
    done: bool,
    /// True once we have descended past this depth into depth+1 at least
    /// once for the current binding (i.e. the leapfrog yielded a match).
    /// When ascending, this signals "advance past the match and re-leapfrog".
    has_descended: bool,
}

impl DepthState {
    fn fresh() -> Self {
        Self {
            p: 0,
            primed: false,
            done: false,
            has_descended: false,
        }
    }
}

pub struct BatchIter<'src> {
    exec: WcojExecutor<'src>,
    builder: BindingBatchBuilder,
    /// One iter per BGP pattern, in BGP order. Always present; never
    /// moved (in contrast to the original LeapfrogJoin ownership scheme).
    iters: Vec<Box<dyn TrieIterator + 'src>>,
    /// For each variable depth: indices of `iters` that mention this var.
    contributing: Vec<Vec<usize>>,
    /// For each variable depth `d`: indices of iters that mention `d` AND
    /// also mention some deeper variable.
    descend_at: Vec<Vec<usize>>,
    /// Iters whose top-contribution depth is exactly d (used for the
    /// "reset on re-entry" pass during ascent).
    top_at: Vec<Vec<usize>>,
    /// Per-depth state. `None` ⇒ not yet entered at this depth.
    state: Vec<Option<DepthState>>,
    /// Current binding per depth.
    binding: Vec<TermId>,
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

        for pat in &exec.bgp.patterns {
            match PatternTrieIter::new(
                exec.source,
                pat,
                &exec.plan.var_order,
                pat.ordering_for(&exec.plan.var_order),
            ) {
                Ok(inner) => {
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
        let mut top_at: Vec<Vec<usize>> = vec![Vec::new(); n_vars];
        for (i, pat) in exec.bgp.patterns.iter().enumerate() {
            for (g, var) in exec.plan.var_order.iter().enumerate() {
                if pat.position_of(*var).is_some() {
                    top_at[g].push(i);
                    break;
                }
            }
        }

        let state = (0..n_vars).map(|_| None).collect();
        let binding = vec![0; n_vars];

        Self {
            exec,
            builder,
            iters,
            contributing,
            descend_at,
            top_at,
            state,
            binding,
            depth: 0,
            finished: false,
            pending_error,
        }
    }

    /// Advance the leapfrog at `depth` until it yields the next common
    /// value (Some) or is exhausted (None). Inline implementation operating
    /// directly on `iters` indexed by `contributing[depth]`.
    fn leapfrog_next(&mut self, depth: u8) -> Option<TermId> {
        let st = self.state[depth as usize].as_mut().unwrap();
        let idxs = &self.contributing[depth as usize];
        let k = idxs.len();
        if st.done || k == 0 {
            st.done = true;
            return None;
        }

        if !st.primed {
            st.primed = true;
            // Initial peek check: any iter exhausted => no match.
            for &i in idxs {
                if self.iters[i].peek(depth).is_none() {
                    st.done = true;
                    return None;
                }
            }
            st.p = 0;
            return self.find_match(depth);
        }

        // Subsequent call: advance the iter that just produced the match
        // past it, then re-leapfrog.
        let cur = self.iters[idxs[st.p]].peek(depth).unwrap();
        self.iters[idxs[st.p]].seek(depth, cur.wrapping_add(1));
        if self.iters[idxs[st.p]].peek(depth).is_none() {
            self.state[depth as usize].as_mut().unwrap().done = true;
            return None;
        }
        let new_p = (st.p + 1) % k;
        self.state[depth as usize].as_mut().unwrap().p = new_p;
        self.find_match(depth)
    }

    fn find_match(&mut self, depth: u8) -> Option<TermId> {
        let idxs = self.contributing[depth as usize].clone();
        let k = idxs.len();
        loop {
            let p = self.state[depth as usize].as_ref().unwrap().p;
            let prev = (p + k - 1) % k;
            let target = self.iters[idxs[prev]].peek(depth)?;
            let cur = self.iters[idxs[p]].peek(depth)?;
            if cur == target {
                return Some(cur);
            }
            self.iters[idxs[p]].seek(depth, target);
            if self.iters[idxs[p]].peek(depth).is_none() {
                self.state[depth as usize].as_mut().unwrap().done = true;
                return None;
            }
            let new_p = (p + 1) % k;
            self.state[depth as usize].as_mut().unwrap().p = new_p;
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

            if self.state[self.depth as usize].is_none() {
                self.state[self.depth as usize] = Some(DepthState::fresh());
            }

            // If the prior iteration descended past this depth and we're
            // back, advance the leapfrog past the prior match before
            // re-leapfrogging.
            let must_advance_after_descent = {
                let st = self.state[self.depth as usize].as_ref().unwrap();
                st.has_descended && !st.done
            };
            if must_advance_after_descent {
                self.state[self.depth as usize]
                    .as_mut()
                    .unwrap()
                    .has_descended = false;
            }

            let next = self.leapfrog_next(self.depth);

            match next {
                Some(v) => {
                    self.binding[self.depth as usize] = v;
                    if self.depth + 1 == n_vars {
                        let flushed = self.builder.push_row(&self.binding);
                        if let Some(b) = flushed {
                            return Some(Ok(b));
                        }
                    } else {
                        for &i in &self.descend_at[self.depth as usize] {
                            self.iters[i].open_level(self.depth);
                        }
                        self.state[self.depth as usize].as_mut().unwrap().has_descended = true;
                        self.depth += 1;
                    }
                }
                None => {
                    // Reset iters whose top-contribution depth is exactly
                    // self.depth so the next re-entry sees fresh state.
                    let to_reset: Vec<usize> = self.top_at[self.depth as usize].clone();
                    for i in to_reset {
                        self.iters[i].reset();
                    }
                    // Drop state at this depth.
                    self.state[self.depth as usize] = None;
                    if self.depth == 0 {
                        self.finished = true;
                        if let Some(b) = self.builder.finish() {
                            return Some(Ok(b));
                        }
                        return None;
                    }
                    self.depth -= 1;
                    for &i in &self.descend_at[self.depth as usize] {
                        self.iters[i].up(self.depth + 1);
                    }
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
