//! Leapfrog Triejoin executor.
//!
//! Drives a depth-first leapfrog intersection over a set of pattern trie
//! iterators. Each iterator exposes *local* variable depths; the executor
//! maintains a per-iterator local-depth cursor that advances/retreats as we
//! descend and ascend.
//!
//! The recursion is implemented with an explicit per-depth state stack so
//! cancellation polling stays cheap. The leapfrog at each depth is inlined
//! against `&mut [AdaptiveIter]` (concrete, not `dyn TrieIterator`) so the
//! peek/seek/open/up calls on the hot path are statically dispatched.

use std::sync::Arc;

use arrow::record_batch::RecordBatch;
use horndb_metrics::metrics;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::Result;
use crate::ids::TermId;
use crate::pattern::{Bgp, TriplePattern};
use crate::plan::ExecutionPlan;
use crate::source::{OrderedTripleIter, TripleSource};
use crate::trie::source_iter::PatternTrieIter;
use crate::trie::TrieIterator;

pub struct WcojExecutor<'src, S: TripleSource + ?Sized + 'src> {
    source: &'src S,
    bgp: Arc<Bgp>,
    plan: Arc<ExecutionPlan>,
    cancel: CancelToken,
}

impl<'src, S: TripleSource + ?Sized + 'src> WcojExecutor<'src, S> {
    pub fn new(source: &'src S, bgp: &Bgp, plan: &ExecutionPlan, cancel: CancelToken) -> Self {
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
    pub fn into_iter(self) -> BatchIter<'src, S> {
        BatchIter::new(self)
    }
}

/// Wrap a `PatternTrieIter` so the leapfrog can address it at the *current
/// global depth* even when that global depth maps to a local depth specific
/// to the iter. The wrapper translates global-depth peek/seek/open/up calls
/// into the iter's local-depth calls.
struct AdaptiveIter<I: OrderedTripleIter> {
    inner: PatternTrieIter<I>,
    /// `g_to_l[global_depth]` = Some(local_depth) if this pattern mentions
    /// the variable at that global depth.
    g_to_l: Vec<Option<u8>>,
}

impl<I: OrderedTripleIter> AdaptiveIter<I> {
    #[inline]
    fn local_for(&self, global_depth: u8) -> u8 {
        self.g_to_l[global_depth as usize]
            .expect("AdaptiveIter queried at a non-contributing global depth")
    }

    /// Re-establish the cursor at `local_for(global_depth)` under the
    /// current parent state. The range at the corresponding phys-level
    /// was last set when an ancestor descent called `open_level` on this
    /// iter, and that ancestor binding hasn't changed since (otherwise
    /// `up(local)` would have torn the range down). So all we need to do
    /// is rewind the cursor to `range[phys].0` — no recomputation of the
    /// range itself.
    fn refresh_for(&mut self, global_depth: u8) {
        let local = self.local_for(global_depth);
        if local == 0 {
            // Top contribution is at this depth — handled by `reset()`.
            return;
        }
        self.inner.rewind_local(local);
    }
}

impl<I: OrderedTripleIter> TrieIterator for AdaptiveIter<I> {
    #[inline]
    fn arity(&self) -> u8 {
        self.inner.arity()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn refresh(&mut self, global_depth: u8) {
        self.refresh_for(global_depth);
    }
    #[inline]
    fn peek(&self, depth: u8) -> Option<TermId> {
        self.inner.peek(self.local_for(depth))
    }
    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let local = self.local_for(depth);
        self.inner.seek(local, value);
    }
    #[inline]
    fn active_run(&mut self, depth: u8) -> Option<&[TermId]> {
        let local = self.local_for(depth);
        self.inner.active_run(local)
    }
    #[inline]
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
    /// True before the first call advances iters.
    primed: bool,
    /// True once the leapfrog is exhausted at this depth.
    done: bool,
    /// True once we have descended past this depth into depth+1 at least
    /// once for the current binding (i.e. the leapfrog yielded a match).
    /// When ascending, this signals "advance past the match and re-leapfrog".
    has_descended: bool,
    /// Armed when `k == 2` and both contributing iters exposed a contiguous
    /// `active_run` at prime time: the whole pairwise intersection was
    /// precomputed once into `simd_buf[depth]` and `find_match` drains it
    /// instead of round-robin seeking. `simd_pos` is the read cursor into
    /// that buffer. Mirrors `LeapfrogJoin`'s `simd_active`/`simd_pos`.
    simd_active: bool,
    simd_pos: usize,
}

impl DepthState {
    fn fresh() -> Self {
        Self {
            p: 0,
            primed: false,
            done: false,
            has_descended: false,
            simd_active: false,
            simd_pos: 0,
        }
    }
}

pub struct BatchIter<'src, S: TripleSource + ?Sized + 'src> {
    exec: WcojExecutor<'src, S>,
    builder: BindingBatchBuilder,
    /// One iter per BGP pattern, in BGP order. Always present; never
    /// moved (in contrast to the original LeapfrogJoin ownership scheme).
    /// Concrete `AdaptiveIter` (not `Box<dyn TrieIterator>`) so the
    /// peek/seek calls on the leapfrog hot path are statically dispatched
    /// — both the outer `AdaptiveIter` impl *and* the inner
    /// `PatternTrieIter` → source-iter dispatch.
    iters: Vec<AdaptiveIter<S::Iter<'src>>>,
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
    /// Per-depth permutation of `contributing[depth]` indices, sorted by
    /// peeked key at prime time so the leapfrog invariant ("iter at
    /// `sorted_idxs[(p+k-1) % k]` holds the running max") holds entering
    /// `find_match`. Hoisted out of `DepthState` so its `Vec` capacity
    /// survives the per-descent state reset and is reused on re-prime.
    sorted_idxs: Vec<Vec<usize>>,
    /// Reusable scratch buffer for the prime-time sort of contributing
    /// iters by current peek. One buffer shared across all depths
    /// because priming is not re-entrant.
    prime_scratch: Vec<(usize, TermId)>,
    /// Per-depth precomputed pairwise intersection for the `k == 2` SIMD
    /// fast path (see `DepthState::simd_active`). One buffer per depth
    /// because the leapfrog is re-entrant across depths; the `Vec` capacity
    /// survives the per-descent state reset and is reused on re-prime.
    simd_buf: Vec<Vec<TermId>>,
    /// Current binding per depth.
    binding: Vec<TermId>,
    depth: u8,
    finished: bool,
    pending_error: Option<crate::error::WcojError>,
    /// Accumulated seek count for Drop→observe. §5.3: plain integer only.
    seeks: u64,
    /// Accumulated leapfrog iteration count for Drop→observe. §5.3: plain integer only.
    iterations: u64,
}

impl<'src, S: TripleSource + ?Sized + 'src> BatchIter<'src, S> {
    fn new(exec: WcojExecutor<'src, S>) -> Self {
        let n_vars = exec.plan.var_order.len();
        let builder = BindingBatchBuilder::new(exec.plan.var_order.clone());
        let mut iters: Vec<AdaptiveIter<S::Iter<'src>>> = Vec::new();
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
                        iters.push(AdaptiveIter { inner, g_to_l });
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
        // Top-contribution depth of each iter: the shallowest var_order index
        // it mentions. A non-ground pattern always mentions ≥1 variable.
        let top_depth: Vec<usize> = nonground_patterns
            .iter()
            .map(|pat| {
                exec.plan
                    .var_order
                    .iter()
                    .position(|var| pat.position_of(*var).is_some())
                    .expect("non-ground pattern mentions no ordered variable")
            })
            .collect();
        let mut top_at: Vec<Vec<usize>> = vec![Vec::new(); n_vars];
        for (i, &d) in top_depth.iter().enumerate() {
            top_at[d].push(i);
        }
        // carry_at[d] = iters in contributing[d] whose top contribution is
        // < d. These need their depth-d state explicitly reset (un-open,
        // re-open) on every re-entry.
        let mut carry_at: Vec<Vec<usize>> = vec![Vec::new(); n_vars];
        for (d, conts) in contributing.iter().enumerate() {
            for &i in conts {
                if top_depth[i] < d {
                    carry_at[d].push(i);
                }
            }
        }

        // If ground patterns ruled out the query, force immediate
        // termination by marking finished.
        let finished = ground_empty;

        let state = (0..n_vars).map(|_| None).collect();
        let binding = vec![0; n_vars];
        let sorted_idxs = (0..n_vars)
            .map(|d| Vec::with_capacity(contributing[d].len()))
            .collect();
        let simd_buf = (0..n_vars).map(|_| Vec::new()).collect();
        let max_k = contributing.iter().map(|c| c.len()).max().unwrap_or(0);

        Self {
            exec,
            builder,
            iters,
            contributing,
            descend_at,
            top_at,
            carry_at,
            state,
            sorted_idxs,
            prime_scratch: Vec::with_capacity(max_k),
            simd_buf,
            binding,
            depth: 0,
            finished,
            pending_error,
            seeks: 0,
            iterations: 0,
        }
    }

    /// Advance the leapfrog at `depth` until it yields the next common
    /// value (Some) or is exhausted (None). Inline implementation operating
    /// directly on `iters` indexed by `contributing[depth]`.
    fn leapfrog_next(&mut self, depth: u8) -> Option<TermId> {
        let d = depth as usize;
        let k = self.contributing[d].len();
        let (done, primed, p) = {
            let st = self.state[d].as_ref().unwrap();
            (st.done, st.primed, st.p)
        };
        if done || k == 0 {
            self.state[d].as_mut().unwrap().done = true;
            return None;
        }

        if !primed {
            // Try the `k == 2` SIMD intersect fast path first: when both
            // contributing iters expose a contiguous `active_run` at prime
            // time (cursors at the level start), precompute the whole
            // intersection once and drain it from `find_match`. Falls through
            // to the scalar round-robin when `active_run` is unavailable
            // (`k != 2`, short run, or no SoA column).
            if k == 2 {
                self.try_arm_simd(depth);
            }
            if self.state[d].as_ref().unwrap().simd_active {
                self.state[d].as_mut().unwrap().primed = true;
                return self.find_match(depth);
            }
            // Sort the contributing iters by their current peek so the
            // classic leapfrog invariant holds: `sorted_idxs[d]` lists
            // iters in non-decreasing key order, `p` starts at the
            // smallest, and `sorted_idxs[d][(p + k - 1) % k]` (i.e.
            // `prev`) always holds the running maximum. Without the
            // sort, a priming snapshot like [A=2, B=14, C=2] would
            // falsely report a match of 2 — the loop only compares
            // `iter[p]` against `iter[prev]`, and would never discover B
            // holds a value the others can't reach.
            self.prime_scratch.clear();
            for j in 0..k {
                let i = self.contributing[d][j];
                match self.iters[i].peek(depth) {
                    None => {
                        let st = self.state[d].as_mut().unwrap();
                        st.done = true;
                        st.primed = true;
                        return None;
                    }
                    Some(v) => self.prime_scratch.push((i, v)),
                }
            }
            // k is small (one entry per pattern at this depth, typically
            // 2-4), so sort_by_key over the inline-allocated scratch is
            // cheap. Reuses `prime_scratch`'s capacity across calls.
            self.prime_scratch.sort_by_key(|&(_, v)| v);
            let sorted = &mut self.sorted_idxs[d];
            sorted.clear();
            sorted.extend(self.prime_scratch.iter().map(|&(i, _)| i));
            let st = self.state[d].as_mut().unwrap();
            st.primed = true;
            st.p = 0;
            return self.find_match(depth);
        }

        // Subsequent call with the SIMD fast path armed: the precomputed
        // intersection already skipped every non-matching candidate, so just
        // drain the next entry — no scalar rotate/seek.
        if self.state[d].as_ref().unwrap().simd_active {
            return self.find_match(depth);
        }

        // Subsequent call: advance the iter that just produced the match
        // past it, then re-leapfrog. Use `sorted_idxs` so the leapfrog
        // invariant (iter at `prev` holds the max) is preserved.
        let cur_iter = self.sorted_idxs[d][p];
        let cur = self.iters[cur_iter].peek(depth).unwrap();
        self.iters[cur_iter].seek(depth, cur.wrapping_add(1));
        self.seeks += 1; // a seek: advance iter past current match
        if self.iters[cur_iter].peek(depth).is_none() {
            self.state[d].as_mut().unwrap().done = true;
            return None;
        }
        self.state[d].as_mut().unwrap().p = (p + 1) % k;
        self.find_match(depth)
    }

    /// Try to arm the `k == 2` SIMD intersect fast path for `depth`. When
    /// both contributing iters expose a contiguous `active_run`, precompute
    /// their intersection once into `simd_buf[depth]`; `find_match` then
    /// drains it. A pairwise accelerator inside the leapfrog: it emits
    /// exactly the same values, in the same (sorted) order, as the scalar
    /// round-robin — `intersect` is symmetric, so the index reordering used
    /// to obtain disjoint borrows does not affect the output. Mirrors
    /// `LeapfrogJoin::try_arm_simd` in `trie/leapfrog.rs`.
    fn try_arm_simd(&mut self, depth: u8) {
        let d = depth as usize;
        debug_assert_eq!(self.contributing[d].len(), 2);
        let i0 = self.contributing[d][0];
        let i1 = self.contributing[d][1];
        // Two distinct iters (a BGP pattern never appears twice in
        // `contributing[d]`); `split_at_mut` hands out the disjoint `&mut`
        // borrows `active_run` needs to materialise both views at once.
        let (lo, hi) = if i0 < i1 { (i0, i1) } else { (i1, i0) };
        let (left, right) = self.iters.split_at_mut(hi);
        let a = match left[lo].active_run(depth) {
            Some(a) => a,
            None => return,
        };
        let b = match right[0].active_run(depth) {
            Some(b) => b,
            None => return,
        };
        let buf = &mut self.simd_buf[d];
        buf.clear();
        horndb_simd::intersect(a, b, buf);
        let st = self.state[d].as_mut().unwrap();
        st.simd_pos = 0;
        st.simd_active = true;
    }

    fn find_match(&mut self, depth: u8) -> Option<TermId> {
        let d = depth as usize;
        // SIMD fast path: drain the precomputed intersection. Each emitted
        // value leaves both cursors positioned at it so the executor's
        // descent (`open_level` on the children) binds the right sub-range —
        // one seek per *emitted* match, not per candidate (the candidate
        // skipping was done in bulk by `intersect`).
        if self.state[d].as_ref().unwrap().simd_active {
            let pos = self.state[d].as_ref().unwrap().simd_pos;
            if pos < self.simd_buf[d].len() {
                let v = self.simd_buf[d][pos];
                self.state[d].as_mut().unwrap().simd_pos = pos + 1;
                let i0 = self.contributing[d][0];
                let i1 = self.contributing[d][1];
                self.iters[i0].seek(depth, v);
                self.iters[i1].seek(depth, v);
                self.seeks += 2;
                return Some(v);
            }
            self.state[d].as_mut().unwrap().done = true;
            return None;
        }
        let k = self.contributing[d].len();
        loop {
            // one leapfrog convergence iteration (plain integer; §5.3)
            self.iterations += 1;
            // Loop invariant (maintained by sorting on prime and by each
            // successful seek making `iter[p]` the new max):
            // `iter[sorted_idxs[d][p+i]]` are non-decreasing in `i` mod k,
            // with the max at `iter[sorted_idxs[d][prev]]`. Hence
            // `iter[p].key == iter[prev].key` implies all iters agree.
            let p = self.state[d].as_ref().unwrap().p;
            let prev = (p + k - 1) % k;
            let iter_prev = self.sorted_idxs[d][prev];
            let iter_p = self.sorted_idxs[d][p];
            let target = self.iters[iter_prev].peek(depth)?;
            let cur = self.iters[iter_p].peek(depth)?;
            if cur == target {
                return Some(cur);
            }
            // Seek iter[p] forward to at least `target`. If the resulting
            // peek is *less than* target (impossible if seek honors >=
            // semantics), bail to avoid infinite loops.
            self.iters[iter_p].seek(depth, target);
            self.seeks += 1; // a seek: advance iter toward target
            match self.iters[iter_p].peek(depth) {
                None => {
                    self.state[d].as_mut().unwrap().done = true;
                    return None;
                }
                Some(v) if v < target => {
                    // Seek violated >= contract; treat as exhausted.
                    self.state[d].as_mut().unwrap().done = true;
                    return None;
                }
                Some(_) => {}
            }
            self.state[d].as_mut().unwrap().p = (p + 1) % k;
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
                let depth = self.depth;
                for &i in &self.carry_at[depth as usize] {
                    self.iters[i].refresh(depth);
                }
                self.state[depth as usize] = Some(DepthState::fresh());
            }

            // If the prior iteration descended past this depth and we're
            // back, advance the leapfrog past the prior match before
            // re-leapfrogging (the advance itself happens in `leapfrog_next`).
            {
                let st = self.state[self.depth as usize].as_mut().unwrap();
                if st.has_descended && !st.done {
                    st.has_descended = false;
                }
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
                    let depth = self.depth;
                    for &i in &self.top_at[depth as usize] {
                        self.iters[i].reset();
                    }
                    // Drop state at this depth.
                    self.state[depth as usize] = None;
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

impl<'src, S: TripleSource + ?Sized + 'src> Iterator for BatchIter<'src, S> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        self.step()
    }
}

impl<'src, S: TripleSource + ?Sized + 'src> Drop for BatchIter<'src, S> {
    fn drop(&mut self) {
        let m = metrics();
        m.wcoj.seeks_per_query.observe(self.seeks as f64);
        m.wcoj.iterations_per_query.observe(self.iterations as f64);
        m.wcoj.peak_iterators.observe(self.iters.len() as f64);
    }
}
