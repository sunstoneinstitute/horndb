//! Predicate-partitioned columnar storage.
//!
//! Each partition is the entire (s, o) pair set for one predicate, stored as
//! two Arrow `UInt64Array` columns in SPO (subject-major) order, with side
//! bitmaps of the distinct subject and object *payloads*.
//!
//! For *hot* predicates (triple count ≥ a configurable threshold) the partition
//! also materialises the object-major `(object, subject)` layout at build time,
//! so all six trie orderings are immediately queryable (SPEC-02 F4). Cold
//! predicates keep only the subject-major layout and materialise the
//! object-major one lazily, on first request, via an internally-synchronised
//! [`OnceLock`].
//!
//! # Runs, and why a partition is not always materialised (HDB-84)
//!
//! A partition is held as a list of **runs** — each run a sorted, deduplicated
//! block of rows — whose concatenation is the partition's row multiset. The
//! merged [`Columns`] every read path needs are built once, on first read, and
//! cached.
//!
//! This is what makes repeated small writes cheap. Appending a batch adds one
//! run and costs `O(batch)`; it does not touch the rows already there. Without
//! it, each write rebuilt the whole partition (copy every existing row forward,
//! re-sort), so N batches into one predicate paid `O(existing)` N times —
//! quadratic in the number of calls. A bulk load driven in batches now pays one
//! sort at the end instead of one per batch.
//!
//! A write still clones the run *list*, and each run carries fixed per-run
//! overhead, so the run count is capped at [`MAX_RUNS`]; reaching it makes the
//! write merge rather than append. The merge itself blocks writers to that
//! partition for its duration — see [`PredicatePartition::cols`].

use crate::ordering::{Ordering, PartitionAxis};
use crate::term::TermId;
use crate::visibility::{visible, CommitVersion, LATEST, UNSET_END};
use arrow::array::{ArrayRef, UInt64Array};
use horndb_metrics::labels::LoadPhase;
use parking_lot::Mutex;
use roaring::RoaringTreemap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Default hot-predicate threshold: predicates with at least this many triples
/// eagerly materialise all six orderings; smaller ones materialise the
/// object-major layout lazily on first request. Configurable per tier — see
/// [`crate::MemoryTier::with_hot_threshold`].
pub const DEFAULT_HOT_THRESHOLD: usize = 1_000_000;

/// Runs a partition may accumulate before a write merges them instead of
/// appending another one.
///
/// Two costs grow with the run count, and this caps both. A write clones the
/// run list, so it is O(runs); and every run carries its own four Arrow arrays
/// and two `RoaringTreemap`s, roughly 1 KiB of fixed overhead however few rows
/// it holds. At this cap that is ~4 MiB of overhead and ~4 k `Arc` clones per
/// write, both negligible.
///
/// It exists for the write pattern that would otherwise be pathological:
/// `Store::insert_triples` called one triple at a time with no read in
/// between, which without a cap builds one run per triple. With the cap such a
/// caller pays a full O(rows) merge every `MAX_RUNS` inserts — bounded memory,
/// and 4,096× less rebuilding than the pre-HDB-84 tier, which merged on every
/// single insert.
///
/// A batched bulk load does not reach it at realistic corpus sizes: 10M
/// triples in 8,192-triple batches is 1,221 runs.
pub(crate) const MAX_RUNS: usize = 4096;

/// Merge one phase into the shared load counters (SPEC-17 §5.4).
fn record_phase(phase: LoadPhase, elapsed: Duration, rows: u64) {
    horndb_metrics::metrics()
        .storage
        .record_load_phase(phase, elapsed, rows);
}

/// The object-major `(object, subject)` columns, sorted by `(object, subject)`.
struct ObjectMajor {
    objects: Arc<UInt64Array>,
    subjects: Arc<UInt64Array>,
    begin: Arc<UInt64Array>,
    end: Arc<UInt64Array>,
}

/// One row: `(subject payload, object payload, begin, end)`.
type Row = (u64, u64, CommitVersion, CommitVersion);

/// A sorted, deduplicated block of rows plus everything derived from them.
/// Serves both as a partition's merged view and as one of its runs.
struct Columns {
    // Subject-major (SPO) columns: rows sorted by (subject, object).
    subjects: Arc<UInt64Array>,
    objects: Arc<UInt64Array>,
    // Per-row visibility stamps, aligned 1:1 with the subject-major columns.
    // `end[i] == UNSET_END` means row i is live. Object-major carries its own
    // re-sorted copies (see `ObjectMajor`).
    begin: Arc<UInt64Array>,
    end: Arc<UInt64Array>,
    // True once any row has a set `end` (a retraction). Lets read paths take a
    // zero-copy fast path when the partition is insert-only.
    has_retractions: bool,
    // The maximum `begin` stamp across all rows (0 for an empty partition).
    // Combined with `has_retractions`, this gates the zero-copy fast path: it
    // is only safe to skip filtering when `at >= max_begin`, i.e. every row's
    // begin bound is already satisfied at the query version `at`.
    max_begin: CommitVersion,
    subject_set: RoaringTreemap,
    object_set: RoaringTreemap,
    // Number of rows with `end == UNSET_END`, frozen at build time. Equal to
    // `len_at(v)` for the version `v` that owns this partition object — see
    // `live_len()` for why that equivalence holds.
    live_len: usize,
}

impl Columns {
    /// Sort `rows` into SPO order and collapse exact-duplicate live rows.
    /// Split from [`Columns::from_sorted_rows`] so a caller merging runs can
    /// release them between the two steps.
    fn sort_dedup(rows: &mut Vec<Row>) {
        // Sort by (subject, object, begin) so the (s, o) columns stay in SPO
        // order for trie iteration; begin orders a tuple's history.
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        // Collapse only exact-duplicate *live* rows for the same (s, o): a
        // repeated insert is a no-op, and the earliest `begin` wins. Dead rows
        // (end set) are history and are kept until compaction.
        rows.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.3 == UNSET_END && b.3 == UNSET_END);
    }

    /// Sort, dedup, and materialise the Arrow columns and side-sets.
    fn from_rows(mut rows: Vec<Row>) -> Self {
        Self::sort_dedup(&mut rows);
        Self::from_sorted_rows(rows)
    }

    /// Materialise columns and side-sets from rows already sorted and
    /// deduplicated by [`Columns::sort_dedup`].
    fn from_sorted_rows(rows: Vec<Row>) -> Self {
        let n = rows.len();
        let mut subject_set = RoaringTreemap::new();
        let mut object_set = RoaringTreemap::new();
        let mut s_col = Vec::with_capacity(n);
        let mut o_col = Vec::with_capacity(n);
        let mut begin_col = Vec::with_capacity(n);
        let mut end_col = Vec::with_capacity(n);
        let mut has_retractions = false;
        let mut max_begin: CommitVersion = 0;
        let mut live_len = 0usize;
        for (s, o, begin, end) in &rows {
            s_col.push(*s);
            o_col.push(*o);
            begin_col.push(*begin);
            end_col.push(*end);
            if *end != UNSET_END {
                has_retractions = true;
            } else {
                live_len += 1;
            }
            if *begin > max_begin {
                max_begin = *begin;
            }
            // Side-sets are supersets across all versions; version-exact sets
            // are computed on demand (Task 4).
            subject_set.insert(TermId(*s).payload());
            object_set.insert(TermId(*o).payload());
        }
        Columns {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            begin: Arc::new(UInt64Array::from(begin_col)),
            end: Arc::new(UInt64Array::from(end_col)),
            has_retractions,
            max_begin,
            subject_set,
            object_set,
            live_len,
        }
    }

    fn len(&self) -> usize {
        self.subjects.len()
    }

    /// Append this block's rows to `out`, in stored order.
    fn extend_rows(&self, out: &mut Vec<Row>) {
        for i in 0..self.len() {
            out.push((
                self.subjects.value(i),
                self.objects.value(i),
                self.begin.value(i),
                self.end.value(i),
            ));
        }
    }

    /// Build the object-major `(object, subject)` columns by re-sorting the
    /// existing subject-major rows by `(object, subject)`.
    fn build_object_major(&self) -> ObjectMajor {
        let n = self.len();
        // `usize` indices, not `u32`: a single hot predicate on LUBM-8000-scale
        // data can exceed `u32::MAX` rows, and narrowing here would silently
        // drop rows from the object-major layout while the subject-major
        // columns still report the full partition.
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_unstable_by(|&a, &b| {
            self.objects
                .value(a)
                .cmp(&self.objects.value(b))
                .then_with(|| self.subjects.value(a).cmp(&self.subjects.value(b)))
        });
        let mut o_col = Vec::with_capacity(n);
        let mut s_col = Vec::with_capacity(n);
        let mut b_col = Vec::with_capacity(n);
        let mut e_col = Vec::with_capacity(n);
        for &i in &idx {
            o_col.push(self.objects.value(i));
            s_col.push(self.subjects.value(i));
            b_col.push(self.begin.value(i));
            e_col.push(self.end.value(i));
        }
        ObjectMajor {
            objects: Arc::new(UInt64Array::from(o_col)),
            subjects: Arc::new(UInt64Array::from(s_col)),
            begin: Arc::new(UInt64Array::from(b_col)),
            end: Arc::new(UInt64Array::from(e_col)),
        }
    }
}

pub struct PredicatePartition {
    // Sorted runs whose concatenation is this partition's rows. Collapsed to a
    // single run — the one shared with `cols` — once `cols` is materialised.
    // The lock is only taken to append a run or to merge; every read path goes
    // through `cols`, which is lock-free after the first read.
    runs: Mutex<Vec<Arc<Columns>>>,
    // The merged, deduplicated view of `runs`. Built on first read.
    cols: OnceLock<Arc<Columns>>,
    // Threshold at which the object-major layout is built eagerly rather than
    // on first object-major request. Carried so a partition grown by
    // `with_appended_rows` keeps its tier's policy.
    hot_threshold: usize,
    // Object-major columns: rows sorted by (object, subject). Eager for hot
    // predicates, otherwise materialised on first `ordered(ObjectMajor)` call.
    object_major: OnceLock<ObjectMajor>,
}

impl PredicatePartition {
    pub fn builder() -> PartitionBuilder {
        PartitionBuilder::default()
    }

    /// The merged view, building it on first call. Every read path goes
    /// through here; after the first call it is an atomic load.
    ///
    /// **This can block a writer.** The merging thread is a reader; it holds
    /// no writer lock, and it holds the `runs` mutex for the whole merge — a
    /// sort of every row in the partition, plus a second sort for the
    /// object-major layout above `hot_threshold`. A concurrent
    /// [`Self::with_appended_rows`] on this same partition waits that out: on
    /// a 10M-row predicate, order of seconds. The work itself is not new (the
    /// pre-HDB-84 tier charged the same sort to the writer on every batch) and
    /// it runs once per partition version, but the thread that pays it has
    /// changed, so a read can now stall a write.
    fn cols(&self) -> &Columns {
        self.cols.get_or_init(|| {
            let mut runs = self.runs.lock();
            let merged = if runs.len() == 1 {
                runs[0].clone()
            } else {
                let t_merge = Instant::now();
                let total: usize = runs.iter().map(|r| r.len()).sum();
                let mut rows: Vec<Row> = Vec::with_capacity(total);
                for run in runs.iter() {
                    run.extend_rows(&mut rows);
                }
                Columns::sort_dedup(&mut rows);
                // Every row now lives in `rows`, so releasing the runs here
                // caps the peak at two copies instead of three. Do it *after*
                // the sort: dropping them earlier would leave `runs` empty if
                // the merge unwound, and a retry would then build an empty
                // partition rather than fail.
                runs.clear();
                let merged = Arc::new(Columns::from_sorted_rows(rows));
                record_phase(LoadPhase::MergeRuns, t_merge.elapsed(), merged.len() as u64);
                merged
            };
            runs.clear();
            runs.push(merged.clone());
            if merged.live_len >= self.hot_threshold {
                let _ = self.object_major.set(merged.build_object_major());
            }
            merged
        })
    }

    /// A new partition holding this one's rows plus `new_rows`, as one extra
    /// run. `O(new_rows + runs)`: the rows already here are shared by `Arc`,
    /// not copied or re-sorted, but the run *list* is cloned. The result is
    /// normally unmaterialised — the first read merges it (see the module
    /// docs) — unless the run count reaches [`MAX_RUNS`], which forces the
    /// merge here so neither the list clone nor the per-run overhead can grow
    /// without bound.
    pub(crate) fn with_appended_rows(&self, new_rows: Vec<Row>) -> PredicatePartition {
        let mut runs = self.runs.lock().clone();
        runs.push(Arc::new(Columns::from_rows(new_rows)));
        let at_cap = runs.len() >= MAX_RUNS;
        let part = PredicatePartition {
            runs: Mutex::new(runs),
            cols: OnceLock::new(),
            hot_threshold: self.hot_threshold,
            object_major: OnceLock::new(),
        };
        if at_cap {
            part.cols();
        }
        part
    }

    pub fn len(&self) -> usize {
        self.cols().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn subjects(&self) -> &UInt64Array {
        &self.cols().subjects
    }

    pub fn objects(&self) -> &UInt64Array {
        &self.cols().objects
    }

    pub fn subjects_arrow(&self) -> ArrayRef {
        self.cols().subjects.clone()
    }

    pub fn objects_arrow(&self) -> ArrayRef {
        self.cols().objects.clone()
    }

    pub fn subject_set(&self) -> &RoaringTreemap {
        &self.cols().subject_set
    }

    pub fn object_set(&self) -> &RoaringTreemap {
        &self.cols().object_set
    }

    /// Distinct subject payloads with at least one row visible at `at`.
    /// Borrows the prebuilt superset when the partition is insert-only AND
    /// every row's `begin` is already `<= at` (so the superset needs no
    /// filtering); otherwise computes the version-exact set.
    pub fn subject_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        let c = self.cols();
        if !c.has_retractions && at >= c.max_begin {
            return std::borrow::Cow::Borrowed(&c.subject_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..c.len() {
            if visible(c.begin.value(i), c.end.value(i), at) {
                set.insert(TermId(c.subjects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }

    /// Distinct object payloads with at least one row visible at `at`.
    /// Borrows the prebuilt superset when the partition is insert-only AND
    /// every row's `begin` is already `<= at`; otherwise computes the
    /// version-exact set.
    pub fn object_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        let c = self.cols();
        if !c.has_retractions && at >= c.max_begin {
            return std::borrow::Cow::Borrowed(&c.object_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..c.len() {
            if visible(c.begin.value(i), c.end.value(i), at) {
                set.insert(TermId(c.objects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }

    /// True if any row in this partition has been retracted (`end` set). When
    /// false, every version-aware read returns the raw columns with no filter.
    pub fn has_retractions(&self) -> bool {
        self.cols().has_retractions
    }

    /// The `begin`/`end` stamp columns (subject-major order), for the WAL and
    /// compaction. Aligned 1:1 with `subjects()`/`objects()`.
    pub fn begins(&self) -> &UInt64Array {
        &self.cols().begin
    }
    pub fn ends(&self) -> &UInt64Array {
        &self.cols().end
    }

    /// Scan the partition in subject-major (SPO) order.
    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        let c = self.cols();
        (0..c.len()).map(move |i| (TermId(c.subjects.value(i)), TermId(c.objects.value(i))))
    }

    /// Scan `(subject, object)` rows visible at `at`, in subject-major order.
    /// Zero-filter fast path when the partition is insert-only AND every
    /// row's `begin` is already `<= at`; otherwise each row is checked
    /// against [`visible`] individually.
    pub fn scan_at(&self, at: CommitVersion) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        let c = self.cols();
        let filtered = c.has_retractions || at < c.max_begin;
        (0..c.len()).filter_map(move |i| {
            if filtered && !visible(c.begin.value(i), c.end.value(i), at) {
                None
            } else {
                Some((TermId(c.subjects.value(i)), TermId(c.objects.value(i))))
            }
        })
    }

    /// Count of rows visible at `at`.
    pub fn len_at(&self, at: CommitVersion) -> usize {
        let c = self.cols();
        if !c.has_retractions && at >= c.max_begin {
            return c.len();
        }
        (0..c.len())
            .filter(|&i| visible(c.begin.value(i), c.end.value(i), at))
            .count()
    }

    /// Number of live rows (`end == UNSET_END`), counted at build time. O(1)
    /// once the partition is materialised.
    ///
    /// Equal to `len_at(at)` only when `at` is the version that built this
    /// partition object or later — which is what a snapshot holding this object
    /// always sees, since copy-on-write hands an older pin a *different, earlier*
    /// object. For such an `at`, every `begin` and every set `end` is `<= at`, so
    /// `visible(begin, end, at) <=> end == UNSET_END`. For an earlier `at`, use
    /// `len_at`.
    pub fn live_len(&self) -> usize {
        self.cols().live_len
    }

    /// Latest-live ordered access (all rows not yet retracted). Convenience for
    /// call sites that always read the newest committed state. See
    /// [`Self::ordered_at`] for the version-aware form and the general
    /// documentation of this access pattern (SPEC-02 F4).
    pub fn ordered(&self, ord: Ordering) -> OrderedColumns {
        self.ordered_at(ord, LATEST)
    }

    /// Ordered access to rows visible at `at`, in any of the six orderings.
    /// Zero-copy when the partition is insert-only AND every row's `begin` is
    /// already `<= at` (raw columns shared by `Arc`); otherwise the visible
    /// subset is materialized once for this call.
    pub fn ordered_at(&self, ord: Ordering, at: CommitVersion) -> OrderedColumns {
        let c = self.cols();
        let (level0, level1, begin, end, axis) = match ord.axis() {
            PartitionAxis::SubjectMajor => (
                c.subjects.clone(),
                c.objects.clone(),
                c.begin.clone(),
                c.end.clone(),
                PartitionAxis::SubjectMajor,
            ),
            PartitionAxis::ObjectMajor => {
                let om = self.object_major.get_or_init(|| c.build_object_major());
                (
                    om.objects.clone(),
                    om.subjects.clone(),
                    om.begin.clone(),
                    om.end.clone(),
                    PartitionAxis::ObjectMajor,
                )
            }
        };
        if !c.has_retractions && at >= c.max_begin {
            return OrderedColumns {
                axis,
                level0,
                level1,
            };
        }
        // Materialize the visible subset, preserving sort order.
        let n = level0.len();
        let mut l0 = Vec::with_capacity(n);
        let mut l1 = Vec::with_capacity(n);
        for i in 0..n {
            if visible(begin.value(i), end.value(i), at) {
                l0.push(level0.value(i));
                l1.push(level1.value(i));
            }
        }
        OrderedColumns {
            axis,
            level0: Arc::new(UInt64Array::from(l0)),
            level1: Arc::new(UInt64Array::from(l1)),
        }
    }

    /// True once the object-major layout has been materialised (eagerly for a
    /// hot predicate, or lazily after the first object-major request).
    pub fn object_major_materialized(&self) -> bool {
        self.object_major.get().is_some()
    }

    /// Estimated in-memory footprint in bytes: 32 bytes per row for the
    /// subject-major axis (16 B for (s, o) + 16 B for (begin, end) stamps),
    /// plus another 32 bytes per row when the object-major layout is
    /// materialised (it carries its own re-sorted (o, s) and (begin, end)
    /// columns). The Roaring side-sets are excluded (small relative to the
    /// columns, and shared shape with the Stage-1 estimate).
    pub fn estimated_bytes(&self) -> u64 {
        let rows = self.len() as u64;
        // 16 B for (s, o) + 16 B for (begin, end) stamps.
        let base = rows * 32;
        if self.object_major_materialized() {
            // Object-major carries its own (o, s) + (begin, end) columns.
            base + rows * 32
        } else {
            base
        }
    }

    /// Subjects whose object column equals `object`, in physical (SPO) order.
    /// Vectorised: the object column is scanned with
    /// [`horndb_simd::filter_indices_eq`] over the contiguous Arrow buffer to
    /// collect matching positions, then [`horndb_simd::gather`] maps those
    /// positions onto the subject column. This is the SIMD-friendly half of the
    /// `rdf:type` partition scan (SPEC-12 F2 / SPEC-02 acceptance #4).
    ///
    /// NOT visibility-filtered: it scans the raw columns regardless of `at`.
    /// Currently only used by a bench and unit tests. Do not call this on a
    /// version-aware read path without first adding a `subjects_with_object_at`
    /// variant.
    pub fn subjects_with_object(&self, object: u64) -> Vec<u64> {
        let c = self.cols();
        let objs: &[u64] = c.objects.values();
        let subs: &[u64] = c.subjects.values();
        let mut positions: Vec<u32> = Vec::new();
        horndb_simd::filter_indices_eq(objs, object, &mut positions);
        let mut subjects = Vec::with_capacity(positions.len());
        horndb_simd::gather(subs, &positions, &mut subjects);
        subjects
    }
}

/// A read-only view of a partition's two stored columns in one ordering's sort
/// order. `level0` is the leading (outer) trie column and `level1` the inner
/// one; both are sorted lexicographically by `(level0, level1)`.
#[derive(Clone)]
pub struct OrderedColumns {
    axis: PartitionAxis,
    level0: Arc<UInt64Array>,
    level1: Arc<UInt64Array>,
}

impl OrderedColumns {
    pub fn axis(&self) -> PartitionAxis {
        self.axis
    }

    pub fn len(&self) -> usize {
        self.level0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The leading (outer) trie column, sorted ascending.
    pub fn level0(&self) -> &UInt64Array {
        &self.level0
    }

    /// The inner trie column, sorted ascending within each `level0` group.
    pub fn level1(&self) -> &UInt64Array {
        &self.level1
    }

    /// Iterate rows as `(level0, level1)` pairs in physical (sorted) order —
    /// the form a trie iterator consumes (outer column leads).
    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        (0..self.len()).map(move |i| (TermId(self.level0.value(i)), TermId(self.level1.value(i))))
    }

    /// Iterate rows as semantic `(subject, object)` pairs, regardless of axis,
    /// preserving this ordering's row order.
    pub fn subject_object(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        let object_major = self.axis == PartitionAxis::ObjectMajor;
        (0..self.len()).map(move |i| {
            let a = TermId(self.level0.value(i));
            let b = TermId(self.level1.value(i));
            if object_major {
                // level0 = object, level1 = subject.
                (b, a)
            } else {
                // level0 = subject, level1 = object.
                (a, b)
            }
        })
    }
}

#[derive(Default)]
pub struct PartitionBuilder {
    rows: Vec<Row>,
}

impl PartitionBuilder {
    /// Append a live row (used by legacy/test call sites that predate stamps):
    /// begin 0, end UNSET_END — visible at every version.
    pub fn append(&mut self, s: TermId, o: TermId) {
        self.rows.push((s.0, o.0, 0, UNSET_END));
    }

    /// Append a row with explicit visibility stamps.
    pub fn append_stamped(
        &mut self,
        s: TermId,
        o: TermId,
        begin: CommitVersion,
        end: CommitVersion,
    ) {
        self.rows.push((s.0, o.0, begin, end));
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// A builder pre-loaded with `(subject, object, begin, end)` rows.
    pub(crate) fn from_rows(rows: Vec<Row>) -> Self {
        Self { rows }
    }

    /// Finalize the partition, eagerly materialising the object-major layout for
    /// a hot predicate (triple count ≥ [`DEFAULT_HOT_THRESHOLD`]).
    pub fn build(self) -> PredicatePartition {
        self.build_with_hot_threshold(DEFAULT_HOT_THRESHOLD)
    }

    /// Finalize the partition. If the deduplicated row count is at least
    /// `hot_threshold`, the object-major layout is materialised eagerly so all
    /// six orderings are immediately queryable; otherwise it is left for lazy
    /// materialisation on first object-major request.
    pub fn build_with_hot_threshold(self, hot_threshold: usize) -> PredicatePartition {
        let cols = Arc::new(Columns::from_rows(self.rows));
        let object_major = OnceLock::new();
        if cols.live_len >= hot_threshold {
            let _ = object_major.set(cols.build_object_major());
        }
        let once = OnceLock::new();
        let _ = once.set(cols.clone());
        PredicatePartition {
            runs: Mutex::new(vec![cols]),
            cols: once,
            hot_threshold,
            object_major,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_len_matches_len_at_own_version_insert_only() {
        // 100 rows, no retractions: live_len must equal len_at(v) for the
        // version the partition was built at. legacy `append` stamps (begin
        // 0, never retracted) are visible at every version, so LATEST is a
        // valid probe.
        let mut b = PartitionBuilder::default();
        for s in 0..100u64 {
            b.append(TermId(s), TermId(s % 5));
        }
        // One duplicate (s, o) pair: dedup_by must collapse it before
        // live_len is counted, not after.
        b.append(TermId(0), TermId(0));
        let part = b.build();
        assert_eq!(part.live_len(), part.len_at(LATEST));
        assert_eq!(part.live_len(), 100);
    }

    #[test]
    fn live_len_matches_len_at_own_version_after_retraction() {
        use crate::memory_tier::MemoryTier;
        use crate::term::DEFAULT_GRAPH;
        use crate::tier::Tier;

        fn id(payload: u64) -> TermId {
            TermId::new(crate::term::TermKind::Uri, payload)
        }

        let tier = MemoryTier::new();
        let pred = id(100);
        let quads: Vec<_> = (0..20u64)
            .map(|s| (DEFAULT_GRAPH, id(s), pred, id(s + 1000)))
            .collect();
        tier.insert_quad_batch(&quads).unwrap();

        // Retract a strict subset (first 7 of the 20).
        let retractions: Vec<_> = quads[..7].to_vec();
        let n = tier.retract_quad_batch(&retractions).unwrap();
        assert_eq!(n, 7, "strict subset retracted");

        let snap = tier.snapshot();
        let version = snap.version();
        let (live, at_version) = snap
            .with_predicate(DEFAULT_GRAPH, pred, |p| (p.live_len(), p.len_at(version)))
            .expect("partition present");
        assert_eq!(live, at_version);
    }

    mod live_len_proptest {
        use super::*;
        use crate::memory_tier::MemoryTier;
        use crate::term::DEFAULT_GRAPH;
        use crate::tier::Tier;
        use proptest::prelude::*;

        fn id(payload: u64) -> TermId {
            TermId::new(crate::term::TermKind::Uri, payload)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// Random interleavings of insert/retract batches over a small id
            /// space (subjects 0..20, across 3 predicates so a batch can touch
            /// several partitions at once). After every batch, `live_len()`
            /// must equal `len_at(version)` for every partition in the
            /// resulting snapshot — the differential referee for the cached
            /// count through arbitrary write sequences.
            #[test]
            fn live_len_matches_len_at_version_after_random_batches(
                ops in proptest::collection::vec(
                    (any::<bool>(), proptest::collection::vec((0u64..20, 0u64..3), 1..8)),
                    1..12,
                )
            ) {
                let tier = MemoryTier::new();
                for (is_insert, rows) in ops {
                    let quads: Vec<_> = rows
                        .iter()
                        .map(|&(s, p)| (DEFAULT_GRAPH, id(s), id(100 + p), id(s + 1000)))
                        .collect();
                    if is_insert {
                        tier.insert_quad_batch(&quads).unwrap();
                    } else {
                        tier.retract_quad_batch(&quads).unwrap();
                    }

                    let snap = tier.snapshot();
                    let version = snap.version();
                    for g in snap.graphs() {
                        for p in snap.predicates(g) {
                            let (live, at_version) = snap
                                .with_predicate(g, p, |part| (part.live_len(), part.len_at(version)))
                                .unwrap();
                            prop_assert_eq!(live, at_version);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scan_objects_equal_matches_scalar() {
        // Build a partition with known (subject, object) rows; object in 0..5.
        let mut b = PartitionBuilder::default();
        for s in 0..100u64 {
            b.append(TermId(s), TermId(s % 5));
        }
        let part = b.build();
        // All subjects whose object == 3, in physical (SPO/ascending) order.
        let want: Vec<u64> = (0..100u64).filter(|s| s % 5 == 3).collect();
        let got = part.subjects_with_object(3);
        assert_eq!(got, want);
    }

    #[test]
    fn scan_objects_equal_no_match_is_empty() {
        let mut b = PartitionBuilder::default();
        for s in 0..10u64 {
            b.append(TermId(s), TermId(s % 5));
        }
        let part = b.build();
        assert!(part.subjects_with_object(42).is_empty());
    }

    #[test]
    fn stamped_scan_filters_by_version() {
        use crate::visibility::UNSET_END;
        let mut b = PartitionBuilder::default();
        // (1,10) inserted at v1, live; (2,20) inserted at v1 then retracted at v3.
        b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
        b.append_stamped(TermId(2), TermId(20), 1, 3);
        let part = b.build();

        // At v2: both visible (retraction not yet in effect).
        let at2: Vec<_> = part.scan_at(2).collect();
        assert_eq!(at2, vec![(TermId(1), TermId(10)), (TermId(2), TermId(20))]);

        // At v3: (2,20) hidden (v3 == end).
        let at3: Vec<_> = part.scan_at(3).collect();
        assert_eq!(at3, vec![(TermId(1), TermId(10))]);

        assert_eq!(part.len_at(2), 2);
        assert_eq!(part.len_at(3), 1);
    }

    #[test]
    fn has_retractions_reports_dead_rows() {
        use crate::visibility::UNSET_END;
        let mut live = PartitionBuilder::default();
        live.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
        assert!(!live.build().has_retractions(), "no dead rows");

        let mut dead = PartitionBuilder::default();
        dead.append_stamped(TermId(1), TermId(10), 1, 2);
        assert!(dead.build().has_retractions(), "one dead row");
    }

    #[test]
    fn ordered_at_filters_both_axes() {
        use crate::ordering::Ordering;
        use crate::visibility::UNSET_END;
        let mut b = PartitionBuilder::default();
        b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
        b.append_stamped(TermId(2), TermId(20), 1, 3); // retracted at v3
        let part = b.build();

        // Object-major (Pos) at v3 must also drop the retracted row.
        let cols = part.ordered_at(Ordering::Pos, 3);
        let rows: Vec<_> = cols.subject_object().collect();
        assert_eq!(rows, vec![(TermId(1), TermId(10))]);

        // At v2 both rows present, object-major sorted by (object, subject).
        let cols2 = part.ordered_at(Ordering::Pos, 2);
        let rows2: Vec<_> = cols2.subject_object().collect();
        assert_eq!(
            rows2,
            vec![(TermId(1), TermId(10)), (TermId(2), TermId(20))]
        );
    }

    #[test]
    fn insert_only_fast_path_still_respects_begin_bound() {
        // Regression for SPEC-25 S1 review Fix 2: an insert-only partition
        // (no retractions, so `has_retractions == false`) with staggered
        // `begin` stamps. The zero-copy fast path must not kick in — and
        // must not return not-yet-visible rows — for an `at` below the
        // partition's max begin.
        use crate::visibility::UNSET_END;
        let mut b = PartitionBuilder::default();
        b.append_stamped(TermId(1), TermId(10), 1, UNSET_END); // visible from v1
        b.append_stamped(TermId(2), TermId(20), 5, UNSET_END); // visible from v5
        let part = b.build();
        assert!(!part.has_retractions(), "insert-only: no retractions");

        // At v3, row (2,20) is not yet inserted — must be excluded.
        let at3: Vec<_> = part.scan_at(3).collect();
        assert_eq!(at3, vec![(TermId(1), TermId(10))]);
        assert_eq!(part.len_at(3), 1);
        assert!(part.subject_set_at(3).contains(TermId(1).payload()));
        assert!(!part.subject_set_at(3).contains(TermId(2).payload()));
        assert!(part.object_set_at(3).contains(TermId(10).payload()));
        assert!(!part.object_set_at(3).contains(TermId(20).payload()));

        let cols = part.ordered_at(crate::ordering::Ordering::Spo, 3);
        let rows: Vec<_> = cols.subject_object().collect();
        assert_eq!(rows, vec![(TermId(1), TermId(10))]);

        // At v5 (== max begin) and later, both rows are visible.
        let at5: Vec<_> = part.scan_at(5).collect();
        assert_eq!(at5, vec![(TermId(1), TermId(10)), (TermId(2), TermId(20))]);
        assert_eq!(part.len_at(5), 2);
    }

    #[test]
    fn object_set_at_drops_retracted_only_payloads() {
        use crate::visibility::UNSET_END;
        let mut b = PartitionBuilder::default();
        b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
        b.append_stamped(TermId(2), TermId(20), 1, 3); // object 20 only via a retracted row
        let part = b.build();

        // At v2 both objects present.
        assert!(part.object_set_at(2).contains(TermId(20).payload()));
        // At v3 object 20 has no visible row → absent from the exact set.
        assert!(!part.object_set_at(3).contains(TermId(20).payload()));
        assert!(part.object_set_at(3).contains(TermId(10).payload()));
    }
}
