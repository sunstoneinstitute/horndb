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
use crate::pattern::{Bgp, TriplePattern};
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

    // Intentionally named `into_iter` for symmetry with
    // `BinaryHashExecutor::into_iter` and for the natural reading at call
    // sites; we deliberately do not implement `IntoIterator` so the
    // executor remains usable without pulling that trait into scope.
    #[allow(clippy::should_implement_trait)]
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

    /// Re-establish the cursor at `local_for(global_depth)` under whatever
    /// the current parent state is. The caller guarantees that
    /// `inner.open_level(prev_local)` has already been called (so the
    /// parent's cursor is positioned). We `inner.up(local)` to clear, then
    /// `inner.open_level(prev_local)` to re-establish. Or, since
    /// `inner.up(local)` only touches phys levels [phys[prev_local]+1 .. phys[local]],
    /// and we then need to walk those same phys levels with open_level
    /// again, we instead call `inner.open_level(prev_local)` directly.
    fn refresh_for(&mut self, global_depth: u8) {
        let local = self.local_for(global_depth);
        if local == 0 {
            // Top contribution is at this depth — handled by `reset()`.
            return;
        }
        // Undo the descent into local, then redo it. This rewinds the
        // cursor at the local-depth level so peek(local) returns the
        // first value under the current ancestor binding.
        self.inner.up(local);
        self.inner.open_level(local - 1);
    }
}

impl<'src> TrieIterator for AdaptiveIter<'src> {
    fn arity(&self) -> u8 {
        self.inner.arity()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn refresh(&mut self, global_depth: u8) {
        self.refresh_for(global_depth);
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
    /// Index into the depth's `contributing` list of the iter currently
    /// being seeked forward. After a seek, this rotates to the next iter
    /// in the circular order. The invariant maintained by `find_match` is
    /// "the iter just *before* `p` in `sorted_idxs` order holds the
    /// running maximum key", so `target` in the loop is always the true
    /// global max across all `k` iters.
    p: usize,
    /// `sorted_idxs[i]` = index into `contributing[depth]` of the `i`-th
    /// iter in non-decreasing key order at prime time. The leapfrog
    /// operates circularly over this list — sorting on prime is what
    /// keeps the "iter[p-1] holds the max, others are ≤ max" invariant
    /// after every seek+rotate. See Veldhuizen, *Leapfrog Triejoin*, 2014.
    sorted_idxs: Vec<usize>,
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
            sorted_idxs: Vec::new(),
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
    /// For each depth d: iters that contribute at d AND whose top
    /// contribution is at some shallower depth (i.e. they "carry" state
    /// from their first descent). On re-entry to depth d (from above)
    /// these need an `inner.up + inner.open_level` reset relative to the
    /// current ancestor bindings.
    carry_at: Vec<Vec<usize>>,
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

        // Ground-pattern pre-check: any all-bound pattern must already
        // match in the source. If not, the join is empty.
        let mut ground_empty = false;
        for pat in &exec.bgp.patterns {
            if !pat.is_ground() {
                continue;
            }
            let mut it = match exec.source.iter(crate::ids::Ordering::Spo) {
                Ok(it) => it,
                Err(e) => {
                    pending_error = Some(e);
                    break;
                }
            };
            let rs = pat.s.as_bound().unwrap();
            let rp = pat.p.as_bound().unwrap();
            let ro = pat.o.as_bound().unwrap();
            it.seek(0, rs);
            if it.peek(0) != Some(rs) {
                ground_empty = true;
                break;
            }
            it.open_level(1);
            it.seek(1, rp);
            if it.peek(1) != Some(rp) {
                ground_empty = true;
                break;
            }
            it.open_level(2);
            it.seek(2, ro);
            if it.peek(2) != Some(ro) {
                ground_empty = true;
                break;
            }
        }

        // Indices into `exec.bgp.patterns` for the non-ground patterns —
        // these are the only patterns we build iters for. `contributing`
        // etc. below are indexed into `iters`, not the original BGP.
        let nonground_pat_idx: Vec<usize> = exec
            .bgp
            .patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_ground())
            .map(|(i, _)| i)
            .collect();
        let nonground_patterns: Vec<&TriplePattern> = nonground_pat_idx
            .iter()
            .map(|&i| &exec.bgp.patterns[i])
            .collect();

        if !ground_empty && pending_error.is_none() {
            for pat in &nonground_patterns {
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
        }

        // Index here is into `iters` (= position in `nonground_patterns`).
        let mut contributing = Vec::with_capacity(n_vars);
        for var in &exec.plan.var_order {
            let mut v = Vec::new();
            for (i, pat) in nonground_patterns.iter().enumerate() {
                if pat.position_of(*var).is_some() {
                    v.push(i);
                }
            }
            contributing.push(v);
        }
        let mut descend_at: Vec<Vec<usize>> = Vec::with_capacity(n_vars);
        for (d, contrib_at_d) in contributing.iter().enumerate().take(n_vars) {
            let mut v = Vec::new();
            for &i in contrib_at_d {
                let pat = nonground_patterns[i];
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
        for (i, pat) in nonground_patterns.iter().enumerate() {
            for (g, var) in exec.plan.var_order.iter().enumerate() {
                if pat.position_of(*var).is_some() {
                    top_at[g].push(i);
                    break;
                }
            }
        }
        // carry_at[d] = iters in contributing[d] whose top contribution is
        // < d. These need their depth-d state explicitly reset (un-open,
        // re-open) on every re-entry.
        let mut carry_at: Vec<Vec<usize>> = vec![Vec::new(); n_vars];
        for (d, conts) in contributing.iter().enumerate() {
            for &i in conts {
                let pat = nonground_patterns[i];
                let top_d = exec
                    .plan
                    .var_order
                    .iter()
                    .position(|var| pat.position_of(*var).is_some())
                    .unwrap();
                if top_d < d {
                    carry_at[d].push(i);
                }
            }
        }

        // If ground patterns ruled out the query, force immediate
        // termination by marking finished.
        let finished = ground_empty;

        let state = (0..n_vars).map(|_| None).collect();
        let binding = vec![0; n_vars];

        Self {
            exec,
            builder,
            iters,
            contributing,
            descend_at,
            top_at,
            carry_at,
            state,
            binding,
            depth: 0,
            finished,
            pending_error,
        }
    }

    /// Advance the leapfrog at `depth` until it yields the next common
    /// value (Some) or is exhausted (None). Inline implementation operating
    /// directly on `iters` indexed by `contributing[depth]`.
    fn leapfrog_next(&mut self, depth: u8) -> Option<TermId> {
        let st = self.state[depth as usize].as_mut().unwrap();
        let k = self.contributing[depth as usize].len();
        if st.done || k == 0 {
            st.done = true;
            return None;
        }

        if !st.primed {
            st.primed = true;
            // Initial peek check: any iter exhausted => no match.
            // Also sort the contributing iters by their current peek so the
            // classic leapfrog invariant holds: `sorted_idxs` lists iters
            // in non-decreasing key order, `p` starts at the smallest, and
            // `sorted_idxs[(p + k - 1) % k]` (i.e. `prev`) always holds the
            // running maximum. Without the sort, a priming snapshot like
            // [A=2, B=14, C=2] would falsely report a match of 2 — the
            // loop only checks `iter[p]` against `iter[prev]`, and would
            // never compare B against the others.
            let mut sorted: Vec<(usize, TermId)> = Vec::with_capacity(k);
            let idxs_snapshot = self.contributing[depth as usize].clone();
            for &i in &idxs_snapshot {
                match self.iters[i].peek(depth) {
                    None => {
                        let st = self.state[depth as usize].as_mut().unwrap();
                        st.done = true;
                        return None;
                    }
                    Some(v) => sorted.push((i, v)),
                }
            }
            sorted.sort_by_key(|&(_, v)| v);
            let sorted_iter_idxs: Vec<usize> = sorted.into_iter().map(|(i, _)| i).collect();
            let st = self.state[depth as usize].as_mut().unwrap();
            st.sorted_idxs = sorted_iter_idxs;
            st.p = 0;
            return self.find_match(depth);
        }

        // Subsequent call: advance the iter that just produced the match
        // past it, then re-leapfrog. Use `sorted_idxs` so the leapfrog
        // invariant (iter at `prev` holds the max) is preserved across
        // calls.
        let cur_iter = st.sorted_idxs[st.p];
        let cur = self.iters[cur_iter].peek(depth).unwrap();
        self.iters[cur_iter].seek(depth, cur.wrapping_add(1));
        if self.iters[cur_iter].peek(depth).is_none() {
            self.state[depth as usize].as_mut().unwrap().done = true;
            return None;
        }
        let new_p = (st.p + 1) % k;
        self.state[depth as usize].as_mut().unwrap().p = new_p;
        self.find_match(depth)
    }

    fn find_match(&mut self, depth: u8) -> Option<TermId> {
        let k = self.contributing[depth as usize].len();
        loop {
            let (sorted_idxs, p) = {
                let st = self.state[depth as usize].as_ref().unwrap();
                (st.sorted_idxs.clone(), st.p)
            };
            let prev = (p + k - 1) % k;
            let target = self.iters[sorted_idxs[prev]].peek(depth)?;
            let cur = self.iters[sorted_idxs[p]].peek(depth)?;
            if cur == target {
                // Loop invariant (maintained by sorting on prime and by
                // each successful seek making `iter[p]` the new max):
                // `iter[sorted_idxs[p+i]]` are non-decreasing in `i` mod k,
                // with the max at `iter[sorted_idxs[prev]]`. Hence
                // `iter[p].key == iter[prev].key` implies all iters at the
                // same key.
                return Some(cur);
            }
            // Seek iter[p] forward to at least `target`. If the resulting
            // peek is *less than* target (impossible if seek honors >= semantics),
            // bail to avoid infinite loops.
            self.iters[sorted_idxs[p]].seek(depth, target);
            let new_cur = self.iters[sorted_idxs[p]].peek(depth);
            match new_cur {
                None => {
                    self.state[depth as usize].as_mut().unwrap().done = true;
                    return None;
                }
                Some(v) if v < target => {
                    // Seek didn't advance — this iter's source violates the
                    // seek contract. Treat as exhausted.
                    self.state[depth as usize].as_mut().unwrap().done = true;
                    return None;
                }
                Some(_) => {}
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
                // Refresh carry iters whose cursor may have been advanced
                // during a deeper leapfrog under a prior ancestor binding.
                // The refresh is also a no-op on first entry — open_level
                // followed by up-then-open_level just re-reads the same
                // range, leaving cursor[..] at lo.
                let carry: Vec<usize> = self.carry_at[self.depth as usize].clone();
                for i in carry {
                    self.iters[i].refresh(self.depth);
                }
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
                        self.state[self.depth as usize]
                            .as_mut()
                            .unwrap()
                            .has_descended = true;
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
