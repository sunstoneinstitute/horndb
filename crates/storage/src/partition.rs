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
}

pub struct PredicatePartition {
    // Subject-major (SPO) columns: rows sorted by (subject, object).
    subjects: Arc<UInt64Array>,
    objects: Arc<UInt64Array>,
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

    /// Scan the partition in subject-major (SPO) order.
    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        (0..self.len()).map(move |i| {
            (
                TermId(self.subjects.value(i)),
                TermId(self.objects.value(i)),
            )
        })
    }

    /// Ordered access to the partition's `(subject, object)` rows for any of the
    /// six trie orderings (SPEC-02 F4).
    ///
    /// Returns [`OrderedColumns`], which holds cheap `Arc` clones of the
    /// underlying Arrow columns and therefore outlives any lock a tier holds
    /// while calling this method. For object-major orderings on a cold
    /// predicate the layout is materialised on the first call and cached; the
    /// materialisation runs once even under concurrent requests
    /// ([`OnceLock`] resolves the race).
    pub fn ordered(&self, ord: Ordering) -> OrderedColumns {
        match ord.axis() {
            PartitionAxis::SubjectMajor => OrderedColumns {
                axis: PartitionAxis::SubjectMajor,
                level0: self.subjects.clone(),
                level1: self.objects.clone(),
            },
            PartitionAxis::ObjectMajor => {
                let om = self.object_major.get_or_init(|| self.build_object_major());
                OrderedColumns {
                    axis: PartitionAxis::ObjectMajor,
                    level0: om.objects.clone(),
                    level1: om.subjects.clone(),
                }
            }
        }
    }

    /// True once the object-major layout has been materialised (eagerly for a
    /// hot predicate, or lazily after the first object-major request).
    pub fn object_major_materialized(&self) -> bool {
        self.object_major.get().is_some()
    }

    /// Estimated in-memory footprint in bytes: 16 bytes per row for the
    /// subject-major columns, plus another 16 bytes per row when the
    /// object-major layout is materialised. The Roaring side-sets are excluded
    /// (small relative to the columns, and shared shape with the Stage-1
    /// estimate).
    pub fn estimated_bytes(&self) -> u64 {
        let rows = self.len() as u64;
        let base = rows * 16;
        if self.object_major_materialized() {
            base + rows * 16
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
        for &i in &idx {
            o_col.push(self.objects.value(i));
            s_col.push(self.subjects.value(i));
        }
        ObjectMajor {
            objects: Arc::new(UInt64Array::from(o_col)),
            subjects: Arc::new(UInt64Array::from(s_col)),
        }
    }

    /// Subjects whose object column equals `object`, in physical (SPO) order.
    /// Vectorised: the object column is scanned with
    /// [`horndb_simd::filter_indices_eq`] over the contiguous Arrow buffer to
    /// collect matching positions, then [`horndb_simd::gather`] maps those
    /// positions onto the subject column. This is the SIMD-friendly half of the
    /// `rdf:type` partition scan (SPEC-12 F2 / SPEC-02 acceptance #4).
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
    pairs: Vec<(u64, u64)>,
}

impl PartitionBuilder {
    pub fn append(&mut self, s: TermId, o: TermId) {
        self.pairs.push((s.0, o.0));
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
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
        // Stage-1: stable sort once at finalize. SPO order ⇒ (subject, object) lexicographic.
        self.pairs.sort_unstable();
        self.pairs.dedup();

        let mut subj_set = RoaringTreemap::new();
        let mut obj_set = RoaringTreemap::new();
        let mut s_col = Vec::with_capacity(self.pairs.len());
        let mut o_col = Vec::with_capacity(self.pairs.len());
        for (s, o) in &self.pairs {
            s_col.push(*s);
            o_col.push(*o);
            subj_set.insert(TermId(*s).payload());
            obj_set.insert(TermId(*o).payload());
        }
        let partition = PredicatePartition {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            subject_set: subj_set,
            object_set: obj_set,
            object_major: OnceLock::new(),
        };
        if partition.len() >= hot_threshold {
            // Eager materialisation; `set` cannot fail on a fresh OnceLock.
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
}
