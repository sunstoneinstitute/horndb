//! `VecTripleSource` — sorted, column-major triple source for the executor.
//!
//! One ordering — the **anchor**, [`ANCHOR_ORDERING`] — is materialised when the
//! source is built. The other five are built on first use, each derived from the
//! anchor. Nothing pays to index an ordering no query asks for (HDB-97): a whole
//! benchmark suite typically touches two or three of the six, and at 10M triples
//! one ordering is ~240 MB and one sort pass.
//!
//! The anchor is `Pso` because that is the order `horndb-storage` already scans
//! in — predicate-major, subject-major — so building it from a store snapshot is
//! a linear pass, not a sort. Any other input order costs one ordinary sort.
//!
//! Orderings can also be maintained in place: [`VecTripleSource::apply_delta`]
//! merges a batch of retracted and inserted triples into the anchor and into
//! whichever other orderings are already built, so a caller holding a cached
//! source need not rebuild it after a small write. An ordering built *after* a
//! delta derives from the already-updated anchor, so both paths agree.
//!
//! Each ordering is stored **column-major** (struct-of-arrays): three
//! `Vec<TermId>`, one per trie level, instead of one `Vec<(TermId, TermId,
//! TermId)>` of rows. A trie level's values are then already contiguous, so the
//! SIMD primitives (`horndb_simd::lower_bound`, `horndb_simd::intersect`) read
//! them directly — no per-level copy out of a strided row layout (SPEC-03 NF2).

use std::sync::OnceLock;

use crate::error::Result;
use crate::ids::{Ordering, TermId, Triple};
use crate::source::{OrderedTripleIter, TripleSource};

/// One ordering's sorted rows, column-major. `levels[d][row]` is that row's
/// value at trie depth `d`; all three columns have the same length.
#[derive(Clone)]
struct TripleColumns {
    levels: [Vec<TermId>; 3],
}

impl TripleColumns {
    /// Sort `rows` (already in the target ordering's axis order), drop
    /// duplicates, and split them into three contiguous columns.
    fn build(mut rows: Vec<(TermId, TermId, TermId)>) -> Self {
        rows.sort_unstable();
        rows.dedup();
        let mut levels = [
            Vec::with_capacity(rows.len()),
            Vec::with_capacity(rows.len()),
            Vec::with_capacity(rows.len()),
        ];
        for (l0, l1, l2) in rows {
            levels[0].push(l0);
            levels[1].push(l1);
            levels[2].push(l2);
        }
        Self { levels }
    }

    /// Derive `to`'s columns from `from`'s already-sorted rows, given the two
    /// orderings share a level-0 axis (`from_ord.level0_axis() ==
    /// to.level0_axis()`).
    ///
    /// Sharing level 0 means the two orderings group rows into the *same*
    /// contiguous blocks — `Pso` and `Pos` are both predicate-major — and differ
    /// only in how rows are ordered inside one. No row crosses a block boundary,
    /// so each block is re-sorted on its own two remaining columns instead of
    /// sorting all n rows: O(n log(n/b)) for b blocks against O(n log n), and a
    /// per-block working set small enough to stay in cache where the global sort
    /// misses (HDB-98).
    ///
    /// `from` is deduplicated, so within one block the remaining two components
    /// are already distinct as a pair — no dedup pass, and the output has
    /// exactly as many rows as the input.
    fn derive_blockwise(from: &TripleColumns, from_ord: Ordering, to: Ordering) -> Self {
        debug_assert_eq!(
            from_ord.level0_axis(),
            to.level0_axis(),
            "blockwise derive needs a shared level-0 axis"
        );
        let n = from.len();
        let mut levels = [
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        ];
        let l0 = &from.levels[0];
        let mut block: Vec<(TermId, TermId)> = Vec::new();
        let mut lo = 0usize;
        while lo < n {
            let key = l0[lo];
            // `l0` is sorted, so the run of rows equal to `key` starts at `lo`
            // and the predicate is monotone over `l0[lo..]` — true then false.
            let hi = lo + l0[lo..].partition_point(|&v| v == key);

            block.clear();
            block.reserve(hi - lo);
            block.extend((lo..hi).map(|i| {
                let [s, p, o] = from_ord.unpermute(l0[i], from.levels[1][i], from.levels[2][i]);
                let [t0, t1, t2] = to.permute(s, p, o);
                debug_assert_eq!(t0, key, "shared level-0 axis must reproduce the block key");
                (t1, t2)
            }));
            block.sort_unstable();

            for &(t1, t2) in &block {
                levels[0].push(key);
                levels[1].push(t1);
                levels[2].push(t2);
            }
            lo = hi;
        }
        Self { levels }
    }

    fn len(&self) -> usize {
        self.levels[0].len()
    }

    /// Heap bytes of the three columns, by allocated capacity.
    fn approx_bytes(&self) -> u64 {
        self.levels
            .iter()
            .map(|l| (l.capacity() * std::mem::size_of::<TermId>()) as u64)
            .sum()
    }

    fn view(&self) -> SortedColumns<'_> {
        debug_assert!(
            self.levels[0].len() == self.levels[1].len()
                && self.levels[1].len() == self.levels[2].len(),
            "all three columns must have the same length"
        );
        SortedColumns {
            levels: [&self.levels[0], &self.levels[1], &self.levels[2]],
        }
    }
}

/// Borrowed column-major view of one ordering's sorted rows.
///
/// The columns are in `ord`'s axis order — the `Triple::by_ordering` layout
/// `from_triples` sorts by. So levels 0/1/2 are `ord`'s `(level0, level1,
/// level2)`: for `Pso` that is `(predicate, subject, object)`; for `Pos` it is
/// `(predicate, object, subject)`.
#[derive(Clone, Copy)]
pub struct SortedColumns<'a> {
    levels: [&'a [TermId]; 3],
}

impl<'a> SortedColumns<'a> {
    /// The whole column at trie depth `level` (0, 1 or 2).
    ///
    /// # Panics
    /// Panics if `level` is not 0, 1 or 2.
    pub fn level(&self, level: usize) -> &'a [TermId] {
        self.levels[level]
    }

    /// Number of rows (identical for all three columns).
    pub fn len(&self) -> usize {
        self.levels[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels[0].is_empty()
    }

    /// Row `i` reassembled as a tuple, in `ord`'s axis order.
    ///
    /// # Panics
    /// Panics if `i` is not less than [`SortedColumns::len`].
    pub fn row(&self, i: usize) -> (TermId, TermId, TermId) {
        (self.levels[0][i], self.levels[1][i], self.levels[2][i])
    }
}

/// The ordering every `VecTripleSource` materialises up front and derives the
/// other five from. `Pso` because `horndb_storage`'s snapshot scan already
/// yields `(predicate, subject, object)` order, so building it is a linear pass.
pub const ANCHOR_ORDERING: Ordering = Ordering::Pso;

// `Clone` lets `HornBackend` pre-warm the `DefaultStrict`/`DefaultUnion` twin
// scope from an already-built source (an O(n) copy of whatever that source has
// materialised) instead of a second `from_triples` rebuild — see
// `wcoj_snapshot` in horn.rs.
#[derive(Clone)]
pub struct VecTripleSource {
    /// [`ANCHOR_ORDERING`]'s sorted, column-major index. Always present; every
    /// other ordering is derived from it.
    anchor: TripleColumns,
    /// The other five orderings, built on first request. Indexed by
    /// [`Ordering::index`]; the anchor's own slot is never filled.
    derived: [OnceLock<TripleColumns>; 6],
    total: usize,
}

impl VecTripleSource {
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        let total = triples.len();
        let rows = triples
            .iter()
            .map(|t| t.by_ordering(ANCHOR_ORDERING))
            .collect();
        Self {
            anchor: TripleColumns::build(rows),
            derived: std::array::from_fn(|_| OnceLock::new()),
            total,
        }
    }

    /// `ord`'s sorted rows, building them from the anchor on first request.
    ///
    /// Concurrent callers racing on the same ordering build it once; the losers
    /// block on `get_or_init` and take the winner's result. Deriving reads only
    /// `anchor`, which is never behind a `OnceLock`, so two threads building
    /// two different orderings cannot deadlock on each other.
    fn columns(&self, ord: Ordering) -> &TripleColumns {
        if ord == ANCHOR_ORDERING {
            return &self.anchor;
        }
        self.derived[ord.index()].get_or_init(|| {
            let a = &self.anchor;
            // `Pos` from a `Pso` anchor: same predicate-major blocks, so sort
            // each block instead of all n rows. See `derive_blockwise`.
            if ord.level0_axis() == ANCHOR_ORDERING.level0_axis() {
                return TripleColumns::derive_blockwise(a, ANCHOR_ORDERING, ord);
            }
            let rows = (0..a.len())
                .map(|i| {
                    let [s, p, o] =
                        ANCHOR_ORDERING.unpermute(a.levels[0][i], a.levels[1][i], a.levels[2][i]);
                    Triple::new(s, p, o).by_ordering(ord)
                })
                .collect();
            TripleColumns::build(rows)
        })
    }

    /// O(log n) membership test against the anchor ordering: narrow to the
    /// predicate's row range, then to the subject's, then look for the object.
    pub fn contains(&self, t: &Triple) -> bool {
        debug_assert_eq!(ANCHOR_ORDERING, Ordering::Pso, "column roles below");
        let cols = &self.anchor;
        let (p_col, s_col, o_col) = (&cols.levels[0], &cols.levels[1], &cols.levels[2]);
        let p_lo = p_col.partition_point(|&v| v < t.p);
        let p_hi = p_lo + p_col[p_lo..].partition_point(|&v| v <= t.p);
        let s_lo = p_lo + s_col[p_lo..p_hi].partition_point(|&v| v < t.s);
        let s_hi = s_lo + s_col[s_lo..p_hi].partition_point(|&v| v <= t.s);
        o_col[s_lo..s_hi].binary_search(&t.o).is_ok()
    }

    /// The snapshot's triples sorted in `ord`, building that ordering if this is
    /// its first use. Read-only view used by `SnapshotStats` to compute
    /// statistics by a single linear scan. See [`SortedColumns`] for the axis
    /// order.
    pub fn sorted_columns(&self, ord: Ordering) -> SortedColumns<'_> {
        self.columns(ord).view()
    }

    /// Approximate heap bytes: the anchor plus every derived ordering built so
    /// far (HDB-146). Six orderings x 3 columns x 8 B is 144 B per triple when
    /// all of them are materialised, which is why this is worth measuring.
    pub fn approx_bytes(&self) -> u64 {
        self.anchor.approx_bytes()
            + self
                .derived
                .iter()
                .filter_map(OnceLock::get)
                .map(TripleColumns::approx_bytes)
                .sum::<u64>()
    }

    /// Which orderings are materialised right now. Test-only window on the
    /// laziness [`Self::columns`] provides.
    #[cfg(test)]
    fn materialised(&self) -> Vec<Ordering> {
        Ordering::ALL
            .into_iter()
            .filter(|&ord| ord == ANCHOR_ORDERING || self.derived[ord.index()].get().is_some())
            .collect()
    }

    /// Apply a delta in place to the anchor and to every other ordering already
    /// materialised, preserving the sorted+deduped invariant. `dels` are removed
    /// if present, `adds` inserted if absent; both are treated as sets. Cost is
    /// O(n + k log k) per materialised ordering, against `from_triples`'s
    /// O(n log n) — the point of the method. An ordering built later derives
    /// from the updated anchor, so it sees the delta too.
    ///
    /// A `del` not present is a no-op. An `add` already present is a no-op — no
    /// duplicate row results. A triple in both `dels` and `adds` ends up
    /// present: delete applies before insert (SPARQL 1.1 §3.1.3, matching the
    /// `apply_quads` batch contract). Duplicates within `dels`, or within
    /// `adds`, are tolerated.
    ///
    /// Unlike `from_triples` (which sets `total` to the pre-dedup input count —
    /// a documented quirk under a multi-graph union, see `union_triples` in
    /// `crates/sparql/src/exec/horn.rs`), `total` after `apply_delta` is the
    /// exact post-merge row count. It is only read by `total_triples()`.
    pub fn apply_delta(&mut self, dels: &[Triple], adds: &[Triple]) {
        if dels.is_empty() && adds.is_empty() {
            // No ordering to rebuild, but `total` can still be stale: it may
            // carry `from_triples`'s pre-dedup over-count (see the doc above).
            // Recompute it here too so "total is exact after apply_delta"
            // holds unconditionally, not just when the delta does work.
            self.total = self.anchor.len();
            return;
        }
        merge_delta(&mut self.anchor, ANCHOR_ORDERING, dels, adds);
        for &ord in &Ordering::ALL {
            if ord == ANCHOR_ORDERING {
                continue;
            }
            if let Some(cols) = self.derived[ord.index()].get_mut() {
                merge_delta(cols, ord, dels, adds);
            }
        }
        self.total = self.anchor.len();
    }
}

/// Merge `dels`/`adds` into one already-sorted ordering, in place. See
/// [`VecTripleSource::apply_delta`], the only caller, for the contract.
fn merge_delta(cols: &mut TripleColumns, ord: Ordering, dels: &[Triple], adds: &[Triple]) {
    let mut dels_sorted: Vec<_> = dels.iter().map(|t| t.by_ordering(ord)).collect();
    dels_sorted.sort_unstable();
    dels_sorted.dedup();
    let mut adds_sorted: Vec<_> = adds.iter().map(|t| t.by_ordering(ord)).collect();
    adds_sorted.sort_unstable();
    adds_sorted.dedup();

    let base_len = cols.levels[0].len();
    let cap = base_len + adds_sorted.len();
    let mut out = [
        Vec::with_capacity(cap),
        Vec::with_capacity(cap),
        Vec::with_capacity(cap),
    ];

    let mut bi = 0usize; // base row index
    let mut di = 0usize; // dels_sorted index
    let mut ai = 0usize; // adds_sorted index
    loop {
        let base_row = if bi < base_len {
            Some((cols.levels[0][bi], cols.levels[1][bi], cols.levels[2][bi]))
        } else {
            None
        };

        // Catch `di` up past any del entries strictly less than the
        // current base row: those values are absent from the
        // (increasing) base — nothing at or after `bi` can match them —
        // so they are no-op deletes. Must run before the equality check
        // below, or a no-op del sitting between two base rows would
        // wedge `di` and hide a later, real match.
        if let Some(b) = base_row {
            while di < dels_sorted.len() && dels_sorted[di] < b {
                di += 1;
            }
        }

        let add_row = adds_sorted.get(ai).copied();

        match (base_row, add_row) {
            (None, None) => break,
            (Some(b), None) => {
                if di < dels_sorted.len() && dels_sorted[di] == b {
                    di += 1;
                } else {
                    push_row(&mut out, b);
                }
                bi += 1;
            }
            (None, Some(a)) => {
                push_row(&mut out, a);
                ai += 1;
            }
            (Some(b), Some(a)) => {
                if b < a {
                    if di < dels_sorted.len() && dels_sorted[di] == b {
                        di += 1;
                    } else {
                        push_row(&mut out, b);
                    }
                    bi += 1;
                } else if a < b {
                    push_row(&mut out, a);
                    ai += 1;
                } else {
                    // b == a: the add wins — delete applies before
                    // insert, so even a del matching this row (if
                    // `dels_sorted[di] == b`, left untouched by the
                    // catch-up above since it only skips values
                    // strictly less than `b`) leaves the row present.
                    // Consume both sides so it is not emitted twice.
                    push_row(&mut out, a);
                    bi += 1;
                    ai += 1;
                }
            }
        }
    }

    cols.levels = out;
}

/// Append `row` to `out`'s three columns, in `by_ordering`'s `(level0, level1,
/// level2)` layout — unless it equals the previously emitted row (dedup guard
/// for the `apply_delta` merge).
#[inline]
fn push_row(out: &mut [Vec<TermId>; 3], row: (TermId, TermId, TermId)) {
    if let Some(&last) = out[0].last() {
        if last == row.0 && out[1].last() == Some(&row.1) && out[2].last() == Some(&row.2) {
            return;
        }
    }
    out[0].push(row.0);
    out[1].push(row.1);
    out[2].push(row.2);
}

impl TripleSource for VecTripleSource {
    type Iter<'a> = VecIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<VecIter<'_>> {
        // Infallible: every ordering is derivable from the anchor, so this
        // source never reports `WcojError::OrderingUnavailable`.
        Ok(VecIter::new(self.columns(ord).view()))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Minimum active-run length for which `active_run` exposes a level to the
/// leapfrog's SIMD `intersect` fast path. Below this, the scalar round-robin
/// seek is cheaper than arming the intersect.
const SIMD_INTERSECT_MIN_RUN: usize = 64;

/// Cursor state: at each depth we hold a `(lo, hi)` row range whose prefix
/// matches the chosen path so far. `cursor[depth]` is the index of the next row
/// to return at `depth`.
pub struct VecIter<'a> {
    /// One contiguous column per trie depth, all of the same length.
    cols: [&'a [TermId]; 3],
    /// (lo, hi) per depth — `hi` is exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
    /// Cached distinct-key copy of the active range at depths 0 and 1, built on
    /// demand by `active_run` (see there for why the leaf needs none).
    distinct: [Option<Vec<TermId>>; 3],
}

impl<'a> VecIter<'a> {
    pub(crate) fn new(cols: SortedColumns<'a>) -> Self {
        let full = (0usize, cols.len());
        Self {
            cols: cols.levels,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
            distinct: [None, None, None],
        }
    }

    #[inline]
    fn col(&self, row: usize, depth: u8) -> TermId {
        self.cols[depth as usize][row]
    }

    /// First absolute index in `[row, hi)` whose `col_depth` column is `> v`,
    /// found by a bounded gallop from `row`. This is the end of the contiguous
    /// run of rows equal to `v` that starts at `row` (rows are sorted and the
    /// parent prefix is fixed, so that run is contiguous).
    ///
    /// Why gallop instead of `partition_point`: on a descent (`open_level`) the
    /// child run is typically short but the parent range is wide — a subject
    /// with 8 objects sitting inside a 400k-row predicate block. A binary
    /// search bisects the *whole* wide range (~log(range) probes scattered
    /// across memory, each a cache miss); an exponential probe from the cursor
    /// reaches the boundary in ~log(run) cache-local steps. It is never
    /// asymptotically worse than binary search (the final window is
    /// binary-searched), so wide runs (e.g. hub subjects) are unaffected.
    #[inline]
    fn run_end(&self, col_depth: u8, row: usize, hi: usize, v: TermId) -> usize {
        let col = self.cols[col_depth as usize];
        let n = hi - row;
        // Gallop a window `[row + lo_off, row + hi_off)` that brackets the
        // boundary: `col(row + lo_off) <= v` stays true, `col(row + hi_off) > v`
        // (or `hi_off == n`).
        let mut lo_off = 0usize;
        let mut step = 1usize;
        while lo_off + step < n && col[row + lo_off + step] <= v {
            lo_off += step;
            step <<= 1;
        }
        let hi_off = (lo_off + step).min(n);
        // Binary-search the bracketed window for the first `col > v`.
        let off = col[row + lo_off..row + hi_off].partition_point(|&c| c <= v);
        row + lo_off + off
    }

    /// Cache-local fast path for the common leapfrog seek: the cursor advances
    /// monotonically, so the target usually sits just past `start`. Probe a
    /// bounded window (≤ `GALLOP_CAP` rows) from the cursor and, if the lower
    /// bound lands inside it, return it exactly. Returns `None` when the target
    /// is farther than the window and data still remains — the caller then runs
    /// the full binary search, so a far ("SPB-style") seek keeps its exact
    /// behaviour and pays only ~log2(cap) extra cache-local probes first.
    ///
    /// The returned index (when `Some`) is identical to `lower_bound` — the
    /// first row in `[start, hi)` whose `depth` column is `>= value`.
    #[inline]
    fn seek_gallop(&self, depth: u8, start: usize, hi: usize, value: TermId) -> Option<usize> {
        const GALLOP_CAP: usize = 64;
        let col = self.cols[depth as usize];
        let n = hi - start;
        if n == 0 {
            return Some(hi);
        }
        if col[start] >= value {
            // Cursor already at/past the target — the overwhelmingly common
            // leapfrog case (peek was already >= the seek target).
            return Some(start);
        }
        // `col(start) < value`. Gallop a window `(lo, hi_off]` bracketing the
        // boundary, capped so a far target bails to the binary search.
        let mut lo = 0usize;
        let mut step = 1usize;
        let hi_off = loop {
            let probe = lo + step;
            if probe >= n {
                break n; // boundary is within `(lo, n)`
            }
            if probe > GALLOP_CAP {
                return None; // far target, data remains → caller binary-searches
            }
            if col[start + probe] >= value {
                break probe; // boundary in `(lo, probe]`
            }
            lo = probe;
            step <<= 1;
        };
        let off = col[start + lo..start + hi_off].partition_point(|&c| c < value);
        Some(start + lo + off)
    }
}

impl<'a> OrderedTripleIter for VecIter<'a> {
    #[inline]
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.col(c, depth))
    }

    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        // Cache-local bounded gallop from the cursor first: the leapfrog seeks
        // monotonically forward, so the target is usually within a few rows.
        // Resolves that case without touching a wide binary search.
        if let Some(idx) = self.seek_gallop(depth, start, hi, value) {
            self.cursor[d] = idx;
            return;
        }
        // Gallop miss: by construction the target is more than `GALLOP_CAP`
        // (64) rows away, so pay the exact lower bound over the rest of the
        // level. Columns are stored contiguously, so this is a direct SIMD
        // `lower_bound` at *every* depth — the old rule that only the depth-0
        // full-data level was worth the SIMD path (and the ~760× `four_cycle`
        // regression from rebuilding a per-`open_level` column) was a property
        // of the row-major layout, which is gone.
        self.cursor[d] = start + horndb_simd::lower_bound(&self.cols[d][start..hi], value);
    }

    #[inline]
    fn open_level(&mut self, depth: u8) {
        assert!((1..=2).contains(&depth), "open_level depth must be 1 or 2");
        let parent = (depth - 1) as usize;
        let (_, hi_parent) = self.range[parent];
        let row = self.cursor[parent];
        let v = self.col(row, depth - 1);
        // Find the half-open range of rows in `[row, hi_parent)` whose
        // depth-(depth-1) column equals `v` AND prefix up to depth-2 matches.
        // Since rows are sorted and the prefix is already constrained, the
        // run with column == v is contiguous. `run_end` gallops from the
        // cursor rather than bisecting the whole (wide) parent range.
        let new_lo = row;
        let new_hi = self.run_end(depth - 1, row, hi_parent, v);
        self.range[depth as usize] = (new_lo, new_hi);
        self.cursor[depth as usize] = new_lo;
        // Drop any distinct-key cache from a previous sibling subtree at this
        // depth; a fresh one is built on demand by `active_run`. Depth 2 never
        // caches (the leaf is handed out as a slice), so there the clear is a
        // no-op.
        self.distinct[depth as usize] = None;
    }

    #[inline]
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            // Root: reset to full data range and rewind cursor to start. The
            // depth-0 distinct cache covers all rows and never changes — keep it.
            self.range[0] = (0, self.cols[0].len());
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
            self.distinct[d] = None;
        }
    }

    #[inline]
    fn rewind(&mut self, depth: u8) {
        let d = depth as usize;
        self.cursor[d] = self.range[d].0;
    }

    #[inline]
    fn active_run_ready(&self, depth: u8) -> bool {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        // Short runs stay scalar and opt out of the SIMD intersect fast path.
        self.cursor[d].max(lo) < hi && hi - lo >= SIMD_INTERSECT_MIN_RUN
    }

    fn active_run(&mut self, depth: u8) -> Option<&[TermId]> {
        if !self.active_run_ready(depth) {
            return None;
        }
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        if d == 2 {
            // Leaf: `open_level(2)` fixed the parent prefix (level0, level1) and
            // the rows are deduplicated triples, so the leaf column over this
            // range is already strictly increasing — exactly the distinct-key
            // contract the leapfrog and `horndb_simd::intersect` require. Hand
            // out the stored column with no copy and no dedup.
            return Some(&self.cols[2][start..hi]);
        }
        // Inner levels: the column repeats a key once per child row (a subject
        // with several objects), but the leapfrog and `intersect` operate on
        // distinct level keys — so dedup into a cached buffer.
        let cursor_val = self.cols[d][start];
        let col = self.cols[d];
        let distinct = self.distinct[d].get_or_insert_with(|| {
            let src = &col[lo..hi];
            let mut out = Vec::with_capacity(src.len());
            for &v in src {
                if out.last() != Some(&v) {
                    out.push(v);
                }
            }
            out
        });
        // Start at the first distinct key >= the key under the cursor (the
        // cursor may have advanced past the level start before arming).
        let off = distinct.partition_point(|&k| k < cursor_val);
        Some(&distinct[off..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split test rows written as tuples into the three stored columns.
    fn columns_of(rows: &[(TermId, TermId, TermId)]) -> [Vec<TermId>; 3] {
        let mut levels = [Vec::new(), Vec::new(), Vec::new()];
        for &(a, b, c) in rows {
            levels[0].push(a);
            levels[1].push(b);
            levels[2].push(c);
        }
        levels
    }

    fn view(levels: &[Vec<TermId>; 3]) -> SortedColumns<'_> {
        SortedColumns {
            levels: [&levels[0], &levels[1], &levels[2]],
        }
    }

    /// Triples whose depth-`depth` column varies over > GALLOP_CAP (64) rows
    /// while the shallower columns are constant, so `open_level` opens the whole
    /// 300-row range at that depth. Used by the seek oracle test to drive every
    /// depth, not just the root.
    fn oracle_triples(depth: u8) -> Vec<Triple> {
        (0..300u64)
            .map(|i| match depth {
                0 => Triple::new(i / 3, i % 3, 0),
                1 => Triple::new(1, i / 3, i % 3),
                _ => Triple::new(1, 1, i),
            })
            .collect()
    }

    /// An iterator with its range opened down to `depth`.
    fn open_to_depth(src: &VecTripleSource, depth: u8) -> VecIter<'_> {
        let mut it = src.iter(Ordering::Spo).expect("Spo ordering");
        for d in 1..=depth {
            it.open_level(d);
        }
        it
    }

    #[test]
    fn run_end_matches_partition_point() {
        // Each column carries runs of varying length inside a wider range; the
        // gallop must land on the same boundary as a straight `partition_point`,
        // at every depth. All three columns are non-decreasing so the
        // whole-column scalar oracle applies to each.
        let rows: Vec<(TermId, TermId, TermId)> = vec![
            (0, 0, 0),
            (0, 0, 1),
            (0, 1, 1), // col0 run of 0 = [0, 3)
            (1, 1, 1), // col0 run of 1 = [3, 4)
            (5, 1, 2),
            (5, 2, 2),
            (5, 2, 3),
            (5, 2, 4), // col0 run of 5 = [4, 8)
            (9, 3, 4), // col0 run of 9 = [8, 9)
        ];
        let levels = columns_of(&rows);
        let n = rows.len();
        let it = VecIter::new(view(&levels));
        for depth in 0..3u8 {
            let col = &levels[depth as usize];
            // For every start row, run_end from that row must equal the scalar
            // partition_point end of the run of `col[row]`.
            for row in 0..n {
                let v = col[row];
                let expect = row + col[row..n].partition_point(|&c| c <= v);
                assert_eq!(it.run_end(depth, row, n, v), expect, "d {depth}, row {row}");
                // A narrower `hi` must clamp the answer.
                for hi in row..=n {
                    let expect_hi = row + col[row..hi].partition_point(|&c| c <= v);
                    assert_eq!(
                        it.run_end(depth, row, hi, v),
                        expect_hi,
                        "d {depth}, row {row}, hi {hi}"
                    );
                }
            }
        }
    }

    #[test]
    fn seek_matches_lower_bound_oracle_near_and_far() {
        // At each depth the opened range spans > GALLOP_CAP (64) rows, so both
        // the gallop-hit (near target) and gallop-miss (far target → SIMD
        // `lower_bound` fallback) paths are exercised.
        for depth in 0..3u8 {
            let d = depth as usize;
            let src = VecTripleSource::from_triples(oracle_triples(depth));
            let cols = src.sorted_columns(Ordering::Spo);
            let (col, n) = (cols.level(d), cols.len());
            let max_key = col[n - 1];
            // For every starting cursor and every target, the post-seek cursor
            // must equal the scalar lower bound over `[start, n)`.
            for &start in &[0usize, 1, 5, 50, 100, 250, n - 1] {
                for value in 0..=(max_key + 2) {
                    let mut it = open_to_depth(&src, depth);
                    assert_eq!(it.range[d], (0, n), "depth {depth} range");
                    it.cursor[d] = start;
                    it.seek(depth, value);
                    let oracle = start + col[start..n].partition_point(|&c| c < value);
                    assert_eq!(it.cursor[d], oracle, "d {depth}, start {start}, v {value}");
                }
            }
        }
    }

    #[test]
    fn active_run_dedups_inner_level_and_skips_to_cursor() {
        // Depth-0 column where each key carries three child rows. The stored
        // column keeps the duplicates; `active_run` must expose distinct keys
        // from the cursor onward. 32 keys × 3 rows clears
        // SIMD_INTERSECT_MIN_RUN.
        let rows: Vec<(TermId, TermId, TermId)> = (0..96u64).map(|i| (i / 3, i % 3, 0)).collect();
        let levels = columns_of(&rows);
        let expect: Vec<TermId> = (0..32u64).collect();

        let mut it = VecIter::new(view(&levels));
        assert_eq!(it.active_run(0).unwrap(), &expect[..]);

        // Cursor parked inside the run of key 7 → keys >= 7.
        let mut it = VecIter::new(view(&levels));
        it.cursor[0] = 7 * 3 + 1;
        assert_eq!(it.active_run(0).unwrap(), &expect[7..]);

        // Short runs opt out of the fast path entirely.
        let short: Vec<(TermId, TermId, TermId)> = (0..10u64).map(|i| (i, 0, 0)).collect();
        let short_levels = columns_of(&short);
        let mut it = VecIter::new(view(&short_levels));
        assert!(it.active_run(0).is_none());
    }

    #[test]
    fn active_run_depth1_cache_is_dropped_on_new_subtree() {
        // The depth-1 distinct-key cache belongs to one depth-0 key's subtree.
        // `open_level` must drop it, or the next sibling subtree is answered
        // from the previous one's keys. Two subjects, each with 80 predicates
        // (>= SIMD_INTERSECT_MIN_RUN) drawn from disjoint key ranges, so a stale
        // cache is unmistakable.
        let mut triples: Vec<Triple> = (0..80u64).map(|p| Triple::new(1, p, 0)).collect();
        triples.extend((100..180u64).map(|p| Triple::new(2, p, 0)));
        let src = VecTripleSource::from_triples(triples);
        let mut it = src.iter(Ordering::Spo).expect("Spo ordering");

        it.open_level(1);
        let first: Vec<TermId> = it.active_run(1).expect("subject 1 armed").to_vec();
        assert_eq!(first, (0..80u64).collect::<Vec<TermId>>());

        // Advance to the next depth-0 key and descend again. No `up(1)` first:
        // `up` also clears the slot, and this test targets `open_level`'s clear.
        it.seek(0, 2);
        it.open_level(1);
        let second = it.active_run(1).expect("subject 2 armed");
        assert_eq!(second, &(100..180u64).collect::<Vec<TermId>>()[..]);
    }

    #[test]
    fn active_run_leaf_is_strictly_increasing_from_cursor() {
        // One subject/predicate prefix with 200 distinct objects: after
        // `open_level(2)` the leaf column is the objects, already distinct.
        let triples: Vec<Triple> = (0..200u64).map(|o| Triple::new(1, 2, o)).collect();
        let src = VecTripleSource::from_triples(triples);
        let mut it = src.iter(Ordering::Spo).expect("Spo ordering");
        it.open_level(1);
        it.open_level(2);
        let run = it
            .active_run(2)
            .expect("leaf run >= SIMD_INTERSECT_MIN_RUN");
        assert_eq!(run.len(), 200);
        assert_eq!(run[0], 0);
        assert!(run.windows(2).all(|w| w[0] < w[1]), "strictly increasing");

        // Near target (inside the 64-row gallop window): the run starts at the
        // cursor's value.
        it.seek(2, 40);
        let run = it.active_run(2).expect("leaf run still armed");
        assert_eq!(run[0], 40);
        assert_eq!(run.len(), 160);
        assert!(run.windows(2).all(|w| w[0] < w[1]), "strictly increasing");

        // Far target (> GALLOP_CAP rows past the cursor): the seek falls through
        // to the SIMD `lower_bound` at the leaf.
        it.seek(2, 150);
        let run = it.active_run(2).expect("leaf run still armed");
        assert_eq!(run[0], 150);
        assert_eq!(run.len(), 50);
    }

    #[test]
    fn contains_finds_present_and_rejects_absent() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ]);
        assert!(src.contains(&Triple::new(1, 2, 4)));
        assert!(!src.contains(&Triple::new(1, 2, 5)));
        assert!(!src.contains(&Triple::new(1, 3, 4)));
        assert!(!src.contains(&Triple::new(9, 9, 9)));
    }

    // -- apply_delta ---------------------------------------------------

    /// One ordering's three columns, snapshotted for comparison.
    type OrderColumns = (Ordering, Vec<TermId>, Vec<TermId>, Vec<TermId>);

    /// Every column of every ordering, for a byte-identical comparison between
    /// `apply_delta`'s result and a full `from_triples` rebuild.
    fn all_columns(src: &VecTripleSource) -> Vec<OrderColumns> {
        Ordering::ALL
            .iter()
            .map(|&ord| {
                let cols = src.sorted_columns(ord);
                (
                    ord,
                    cols.level(0).to_vec(),
                    cols.level(1).to_vec(),
                    cols.level(2).to_vec(),
                )
            })
            .collect()
    }

    /// Small deterministic xorshift64 PRNG — no `rand` dependency, per the
    /// task brief. Not cryptographic; only needs to be reproducible.
    struct Xorshift64(u64);

    impl Xorshift64 {
        fn new(seed: u64) -> Self {
            // xorshift64 is undefined at state 0; fold the seed away from it.
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// Uniform value in `[0, n)`. `n` must be > 0.
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }
    }

    /// A random triple drawn from a `[0, domain)` term-id domain, small enough
    /// that collisions and duplicates actually occur.
    fn random_triple(rng: &mut Xorshift64, domain: u64) -> Triple {
        Triple::new(rng.below(domain), rng.below(domain), rng.below(domain))
    }

    /// Random `dels`/`adds` for one delta round, biased so ~half draw from
    /// the current expected set (present deletes, already-present adds are
    /// the common real-world case) and ~half are arbitrary domain triples.
    fn random_delta_half(
        rng: &mut Xorshift64,
        cur: &[Triple],
        len: usize,
        domain: u64,
    ) -> Vec<Triple> {
        (0..len)
            .map(|_| {
                if !cur.is_empty() && rng.below(2) == 0 {
                    cur[rng.below(cur.len() as u64) as usize]
                } else {
                    random_triple(rng, domain)
                }
            })
            .collect()
    }

    #[test]
    fn apply_delta_matches_full_rebuild() {
        use std::collections::HashSet;

        let mut rng = Xorshift64::new(0xC0FFEE_u64);
        const DOMAIN: u64 = 8; // small domain: 512 possible triples

        for iter in 0..200 {
            // Force the empty-base path on the first iteration: at this
            // seed `rng.below(201)` never draws 0 in 200 rounds on its own,
            // so the empty-base branch would otherwise go untested here (a
            // dedicated test covers it too, but forcing it here keeps this
            // test's coverage independent of seed luck).
            let base_len = if iter == 0 {
                0
            } else {
                rng.below(201) as usize
            };
            let base: Vec<Triple> = (0..base_len)
                .map(|_| random_triple(&mut rng, DOMAIN))
                .collect();

            let mut expected: HashSet<Triple> = base.iter().copied().collect();
            let mut src = VecTripleSource::from_triples(base);

            // Two delta rounds in sequence: the second lands on a source
            // that is itself `apply_delta`'s own output, exercising the
            // chained-delta shape production code hits on every mutation
            // after the first (Task 3 wires this in behind every SPARQL
            // Update). `cur` is a fresh snapshot of the *current* expected
            // set each round, not the original base — later rounds bias
            // toward triples the previous round just settled on.
            for round in 0..2 {
                let cur: Vec<Triple> = expected.iter().copied().collect();
                let del_len = rng.below(30) as usize;
                let dels = random_delta_half(&mut rng, &cur, del_len, DOMAIN);
                let add_len = rng.below(30) as usize;
                let adds = random_delta_half(&mut rng, &cur, add_len, DOMAIN);

                // Expected set, computed independently: delete before insert.
                for d in &dels {
                    expected.remove(d);
                }
                for a in &adds {
                    expected.insert(*a);
                }

                src.apply_delta(&dels, &adds);

                let want = VecTripleSource::from_triples(expected.iter().copied().collect());

                assert_eq!(
                    src.total_triples(),
                    expected.len(),
                    "iter {iter} round {round}: total mismatch"
                );
                for &ord in &Ordering::ALL {
                    let got_cols = src.sorted_columns(ord);
                    let want_cols = want.sorted_columns(ord);
                    assert_eq!(
                        got_cols.level(0),
                        want_cols.level(0),
                        "iter {iter} round {round}, ord {ord:?}, level 0"
                    );
                    assert_eq!(
                        got_cols.level(1),
                        want_cols.level(1),
                        "iter {iter} round {round}, ord {ord:?}, level 1"
                    );
                    assert_eq!(
                        got_cols.level(2),
                        want_cols.level(2),
                        "iter {iter} round {round}, ord {ord:?}, level 2"
                    );
                }
            }
        }
    }

    #[test]
    fn only_the_anchor_ordering_is_built_up_front() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ]);
        assert_eq!(src.materialised(), vec![ANCHOR_ORDERING]);
    }

    #[test]
    fn an_ordering_is_built_on_first_request_and_then_reused() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ]);

        let first = src.sorted_columns(Ordering::Ops).level(0).as_ptr();
        assert_eq!(src.materialised(), vec![ANCHOR_ORDERING, Ordering::Ops]);

        // Same allocation on the second request: `get_or_init` built it once.
        assert_eq!(src.sorted_columns(Ordering::Ops).level(0).as_ptr(), first);
        assert_eq!(src.materialised(), vec![ANCHOR_ORDERING, Ordering::Ops]);
    }

    #[test]
    fn an_ordering_built_after_a_delta_matches_one_merged_through_it() {
        let base = vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ];
        let dels = [Triple::new(1, 2, 4)];
        let adds = [Triple::new(9, 2, 1), Triple::new(1, 2, 3)];

        // `merged` has every ordering built before the delta, so each one is
        // updated in place. `derived` has none, so each is built afterwards
        // from the already-updated anchor. The two must agree.
        let mut merged = VecTripleSource::from_triples(base.clone());
        let _ = all_columns(&merged);
        merged.apply_delta(&dels, &adds);

        let mut derived = VecTripleSource::from_triples(base);
        derived.apply_delta(&dels, &adds);

        assert_eq!(derived.materialised(), vec![ANCHOR_ORDERING]);
        assert_eq!(all_columns(&derived), all_columns(&merged));
        assert_eq!(derived.total_triples(), merged.total_triples());
    }

    #[test]
    fn apply_delta_empty_is_noop() {
        // Distinct triples: `from_triples`'s `total` already equals the
        // deduped count here, so recomputing `total` on the empty-delta
        // early return happens to leave it unchanged too. That's incidental
        // to *this* fixture, not a general guarantee — see
        // `apply_delta_empty_delta_recomputes_total_for_duplicate_input` for
        // the case where it does change.
        let triples = vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ];
        let mut src = VecTripleSource::from_triples(triples);
        let before = all_columns(&src);

        src.apply_delta(&[], &[]);

        assert_eq!(all_columns(&src), before, "columns must be unchanged");
    }

    #[test]
    fn apply_delta_empty_delta_recomputes_total_for_duplicate_input() {
        // `from_triples` sets `total = triples.len()` before its internal
        // dedup, so duplicate input over-counts (a documented quirk — see
        // the `apply_delta` doc and `union_triples` in
        // `crates/sparql/src/exec/horn.rs`). Even a no-op `apply_delta(&[],
        // &[])` must still bring `total` down to the exact row count, since
        // the doc promises "total after apply_delta is exact"
        // unconditionally, not just when the delta does work.
        let t = Triple::new(1, 2, 3);
        let mut src = VecTripleSource::from_triples(vec![t, t]);
        assert_eq!(
            src.total_triples(),
            2,
            "from_triples over-counts duplicate input by design"
        );

        src.apply_delta(&[], &[]);

        assert_eq!(
            src.total_triples(),
            1,
            "apply_delta must recompute total exactly, even for an empty delta"
        );
    }

    #[test]
    fn apply_delta_delete_absent_and_insert_present_are_noops() {
        let triples = vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ];
        let mut src = VecTripleSource::from_triples(triples);
        let before = all_columns(&src);
        let before_total = src.total_triples();

        // Delete something never present, insert something already present.
        src.apply_delta(&[Triple::new(9, 9, 9)], &[Triple::new(1, 2, 3)]);

        assert_eq!(all_columns(&src), before);
        assert_eq!(src.total_triples(), before_total);
    }

    #[test]
    fn apply_delta_same_triple_deleted_and_added_stays_present() {
        let t = Triple::new(1, 2, 3);
        let mut src = VecTripleSource::from_triples(vec![t, Triple::new(5, 6, 7)]);

        src.apply_delta(&[t], &[t]);

        assert!(src.contains(&t));
        assert_eq!(src.total_triples(), 2);
        for &ord in &Ordering::ALL {
            let cols = src.sorted_columns(ord);
            assert_eq!(cols.len(), 2, "ord {ord:?}");
        }

        // Also from an empty base: del-before-insert on a triple that was
        // never there still leaves it present.
        let mut empty = VecTripleSource::from_triples(vec![]);
        empty.apply_delta(&[t], &[t]);
        assert!(empty.contains(&t));
        assert_eq!(empty.total_triples(), 1);
    }

    #[test]
    fn apply_delta_to_empty_base_matches_from_triples() {
        let adds = vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 3), // duplicate within adds, tolerated
            Triple::new(4, 5, 6),
        ];
        let mut src = VecTripleSource::from_triples(vec![]);
        src.apply_delta(&[], &adds);

        let want = VecTripleSource::from_triples(vec![Triple::new(1, 2, 3), Triple::new(4, 5, 6)]);
        assert_eq!(all_columns(&src), all_columns(&want));
        assert_eq!(src.total_triples(), 2);
    }

    // -- blockwise derive (HDB-98) -------------------------------------

    /// The shared-level-0 fast path must produce byte-identical columns to the
    /// global-sort derive it replaces, over a domain dense enough to give many
    /// multi-row predicate blocks and many duplicate triples.
    #[test]
    fn blockwise_derive_matches_global_sort_derive() {
        let mut rng = Xorshift64::new(0xB10C_5017);
        const DOMAIN: u64 = 6; // 216 possible triples, ~6 rows per predicate

        for iter in 0..50 {
            let n = 1 + rng.below(400) as usize;
            let triples: Vec<Triple> = (0..n).map(|_| random_triple(&mut rng, DOMAIN)).collect();
            let anchor = TripleColumns::build(
                triples
                    .iter()
                    .map(|t| t.by_ordering(ANCHOR_ORDERING))
                    .collect(),
            );

            for &ord in &Ordering::ALL {
                if ord.level0_axis() != ANCHOR_ORDERING.level0_axis() {
                    continue;
                }
                let want =
                    TripleColumns::build(triples.iter().map(|t| t.by_ordering(ord)).collect());
                let got = TripleColumns::derive_blockwise(&anchor, ANCHOR_ORDERING, ord);
                assert_eq!(got.levels, want.levels, "iter {iter}, ord {ord:?}");
            }
        }
    }

    /// An empty anchor derives to an empty ordering rather than panicking on
    /// the block scan.
    #[test]
    fn blockwise_derive_of_empty_anchor_is_empty() {
        let anchor = TripleColumns::build(vec![]);
        let got = TripleColumns::derive_blockwise(&anchor, ANCHOR_ORDERING, Ordering::Pos);
        assert_eq!(got.len(), 0);
    }

    #[test]
    fn apply_delta_removing_everything_leaves_all_orderings_empty() {
        let triples = vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ];
        let dels = triples.clone();
        let mut src = VecTripleSource::from_triples(triples);

        src.apply_delta(&dels, &[]);

        assert_eq!(src.total_triples(), 0);
        for &ord in &Ordering::ALL {
            let cols = src.sorted_columns(ord);
            assert!(cols.is_empty(), "ord {ord:?}");
            // No panic on iteration over an emptied ordering.
            let it = src.iter(ord).expect("ordering exists");
            assert_eq!(it.peek(0), None);
        }
    }
}
