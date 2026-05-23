//! Predicate-partitioned columnar storage.
//!
//! Each partition is the entire (s, o) pair set for one predicate, stored as
//! two Arrow `UInt64Array` columns in SPO order, with side bitmaps of the
//! distinct subject and object *payloads*.

use crate::term::TermId;
use arrow::array::{ArrayRef, UInt64Array};
use roaring::RoaringTreemap;
use std::sync::Arc;

pub struct PredicatePartition {
    subjects: Arc<UInt64Array>,
    objects: Arc<UInt64Array>,
    subject_set: RoaringTreemap,
    object_set: RoaringTreemap,
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

    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        (0..self.len()).map(move |i| {
            (
                TermId(self.subjects.value(i)),
                TermId(self.objects.value(i)),
            )
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

    pub fn build(mut self) -> PredicatePartition {
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
        PredicatePartition {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            subject_set: subj_set,
            object_set: obj_set,
        }
    }
}
