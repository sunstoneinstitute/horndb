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

use crate::ordering::{Ordering, PartitionAxis};
use crate::term::TermId;
use crate::visibility::{visible, CommitVersion, LATEST, UNSET_END};
use arrow::array::{ArrayRef, UInt64Array};
use roaring::RoaringTreemap;
use std::sync::{Arc, OnceLock};

/// Default hot-predicate threshold: predicates with at least this many triples
/// eagerly materialise all six orderings; smaller ones materialise the
/// object-major layout lazily on first request. Configurable per tier — see
/// [`crate::MemoryTier::with_hot_threshold`].
pub const DEFAULT_HOT_THRESHOLD: usize = 1_000_000;

/// The object-major `(object, subject)` columns, sorted by `(object, subject)`.
struct ObjectMajor {
    objects: Arc<UInt64Array>,
    subjects: Arc<UInt64Array>,
    begin: Arc<UInt64Array>,
    end: Arc<UInt64Array>,
}

pub struct PredicatePartition {
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
    // Object-major columns: rows sorted by (object, subject). Eager for hot
    // predicates, otherwise materialised on first `ordered(ObjectMajor)` call.
    object_major: OnceLock<ObjectMajor>,
}

impl PredicatePartition {
    pub fn builder() -> PartitionBuilder {
        PartitionBuilder::default()
    }

    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn subjects(&self) -> &UInt64Array {
        &self.subjects
    }

    pub fn objects(&self) -> &UInt64Array {
        &self.objects
    }

    pub fn subjects_arrow(&self) -> ArrayRef {
        self.subjects.clone()
    }

    pub fn objects_arrow(&self) -> ArrayRef {
        self.objects.clone()
    }

    pub fn subject_set(&self) -> &RoaringTreemap {
        &self.subject_set
    }

    pub fn object_set(&self) -> &RoaringTreemap {
        &self.object_set
    }

    /// Distinct subject payloads with at least one row visible at `at`.
    /// Borrows the prebuilt superset when the partition is insert-only AND
    /// every row's `begin` is already `<= at` (so the superset needs no
    /// filtering); otherwise computes the version-exact set.
    pub fn subject_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        if !self.has_retractions && at >= self.max_begin {
            return std::borrow::Cow::Borrowed(&self.subject_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..self.len() {
            if visible(self.begin.value(i), self.end.value(i), at) {
                set.insert(TermId(self.subjects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }

    /// Distinct object payloads with at least one row visible at `at`.
    /// Borrows the prebuilt superset when the partition is insert-only AND
    /// every row's `begin` is already `<= at`; otherwise computes the
    /// version-exact set.
    pub fn object_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        if !self.has_retractions && at >= self.max_begin {
            return std::borrow::Cow::Borrowed(&self.object_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..self.len() {
            if visible(self.begin.value(i), self.end.value(i), at) {
                set.insert(TermId(self.objects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }

    /// True if any row in this partition has been retracted (`end` set). When
    /// false, every version-aware read returns the raw columns with no filter.
    pub fn has_retractions(&self) -> bool {
        self.has_retractions
    }

    /// The `begin`/`end` stamp columns (subject-major order), for the WAL and
    /// compaction. Aligned 1:1 with `subjects()`/`objects()`.
    pub fn begins(&self) -> &UInt64Array {
        &self.begin
    }
    pub fn ends(&self) -> &UInt64Array {
        &self.end
    }

    /// Scan the partition in subject-major (SPO) order.
    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        (0..self.len()).map(move |i| {
            (
                TermId(self.subjects.value(i)),
                TermId(self.objects.value(i)),
            )
        })
    }

    /// Scan `(subject, object)` rows visible at `at`, in subject-major order.
    /// Zero-filter fast path when the partition is insert-only AND every
    /// row's `begin` is already `<= at`; otherwise each row is checked
    /// against [`visible`] individually.
    pub fn scan_at(&self, at: CommitVersion) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        let filtered = self.has_retractions || at < self.max_begin;
        (0..self.len()).filter_map(move |i| {
            if filtered && !visible(self.begin.value(i), self.end.value(i), at) {
                None
            } else {
                Some((
                    TermId(self.subjects.value(i)),
                    TermId(self.objects.value(i)),
                ))
            }
        })
    }

    /// Count of rows visible at `at`.
    pub fn len_at(&self, at: CommitVersion) -> usize {
        if !self.has_retractions && at >= self.max_begin {
            return self.len();
        }
        (0..self.len())
            .filter(|&i| visible(self.begin.value(i), self.end.value(i), at))
            .count()
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
        let (level0, level1, begin, end, axis) = match ord.axis() {
            PartitionAxis::SubjectMajor => (
                self.subjects.clone(),
                self.objects.clone(),
                self.begin.clone(),
                self.end.clone(),
                PartitionAxis::SubjectMajor,
            ),
            PartitionAxis::ObjectMajor => {
                let om = self.object_major.get_or_init(|| self.build_object_major());
                (
                    om.objects.clone(),
                    om.subjects.clone(),
                    om.begin.clone(),
                    om.end.clone(),
                    PartitionAxis::ObjectMajor,
                )
            }
        };
        if !self.has_retractions && at >= self.max_begin {
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
        let objs: &[u64] = self.objects.values();
        let subs: &[u64] = self.subjects.values();
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
    // (subject, object, begin, end) rows.
    rows: Vec<(u64, u64, CommitVersion, CommitVersion)>,
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

    /// Finalize the partition, eagerly materialising the object-major layout for
    /// a hot predicate (triple count ≥ [`DEFAULT_HOT_THRESHOLD`]).
    pub fn build(self) -> PredicatePartition {
        self.build_with_hot_threshold(DEFAULT_HOT_THRESHOLD)
    }

    /// Finalize the partition. If the deduplicated row count is at least
    /// `hot_threshold`, the object-major layout is materialised eagerly so all
    /// six orderings are immediately queryable; otherwise it is left for lazy
    /// materialisation on first object-major request.
    pub fn build_with_hot_threshold(mut self, hot_threshold: usize) -> PredicatePartition {
        // Sort by (subject, object, begin) so the (s, o) columns stay in SPO
        // order for trie iteration; begin orders a tuple's history.
        self.rows
            .sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        // Collapse only exact-duplicate *live* rows for the same (s, o): a
        // repeated insert is a no-op. Dead rows (end set) are history and are
        // kept until compaction.
        self.rows
            .dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.3 == UNSET_END && b.3 == UNSET_END);

        let n = self.rows.len();
        let mut subj_set = RoaringTreemap::new();
        let mut obj_set = RoaringTreemap::new();
        let mut s_col = Vec::with_capacity(n);
        let mut o_col = Vec::with_capacity(n);
        let mut begin_col = Vec::with_capacity(n);
        let mut end_col = Vec::with_capacity(n);
        let mut has_retractions = false;
        let mut max_begin: CommitVersion = 0;
        for (s, o, begin, end) in &self.rows {
            s_col.push(*s);
            o_col.push(*o);
            begin_col.push(*begin);
            end_col.push(*end);
            if *end != UNSET_END {
                has_retractions = true;
            }
            if *begin > max_begin {
                max_begin = *begin;
            }
            // Side-sets are supersets across all versions; version-exact sets
            // are computed on demand (Task 4).
            subj_set.insert(TermId(*s).payload());
            obj_set.insert(TermId(*o).payload());
        }
        let partition = PredicatePartition {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            begin: Arc::new(UInt64Array::from(begin_col)),
            end: Arc::new(UInt64Array::from(end_col)),
            has_retractions,
            max_begin,
            subject_set: subj_set,
            object_set: obj_set,
            object_major: OnceLock::new(),
        };
        if partition.len_at(LATEST) >= hot_threshold {
            let _ = partition.object_major.set(partition.build_object_major());
        }
        partition
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
