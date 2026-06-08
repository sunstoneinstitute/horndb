//! Index orderings for triple access (SPEC-02 F4).
//!
//! HornDB partitions triples by predicate, so within a single
//! [`crate::partition::PredicatePartition`] the predicate component is
//! constant. The six global trie orderings therefore collapse to **two**
//! distinct physical layouts of the stored `(subject, object)` columns:
//!
//! | Global ordering | Component priority (P constant) | Physical axis |
//! |-----------------|---------------------------------|---------------|
//! | `Spo`, `Sop`, `Pso` | subject before object           | [`PartitionAxis::SubjectMajor`] |
//! | `Pos`, `Osp`, `Ops` | object before subject           | [`PartitionAxis::ObjectMajor`]  |
//!
//! The subject-major layout is the one a partition always materialises (it is
//! the SPO order the builder sorts into at finalize time). The object-major
//! layout is materialised eagerly for *hot* predicates and lazily, on first
//! request, for cold ones — see [`crate::partition::PredicatePartition::ordered`].

/// One of the six trie orderings, named `<level0><level1><level2>`.
///
/// This mirrors the executor-side ordering enum in `horndb-wcoj`, but is
/// defined here because `horndb-storage` is the lower crate in the dependency
/// order and cannot depend on `horndb-wcoj`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordering {
    Spo,
    Sop,
    Pso,
    Pos,
    Osp,
    Ops,
}

impl Ordering {
    /// All six orderings, for exhaustive iteration in tests and acceptance checks.
    pub const ALL: [Ordering; 6] = [
        Ordering::Spo,
        Ordering::Sop,
        Ordering::Pso,
        Ordering::Pos,
        Ordering::Osp,
        Ordering::Ops,
    ];

    /// The physical partition layout that serves this ordering.
    ///
    /// Because the predicate is constant within a partition, only the relative
    /// order of the subject and object components matters: orderings that place
    /// the subject before the object are served by the subject-major columns,
    /// the rest by the object-major columns.
    pub fn axis(self) -> PartitionAxis {
        match self {
            Ordering::Spo | Ordering::Sop | Ordering::Pso => PartitionAxis::SubjectMajor,
            Ordering::Pos | Ordering::Osp | Ordering::Ops => PartitionAxis::ObjectMajor,
        }
    }
}

/// The physical column layout of a predicate partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartitionAxis {
    /// Rows sorted by `(subject, object)` — the SPO layout, always present.
    SubjectMajor,
    /// Rows sorted by `(object, subject)` — materialised on demand.
    ObjectMajor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_mapping_matches_component_priority() {
        assert_eq!(Ordering::Spo.axis(), PartitionAxis::SubjectMajor);
        assert_eq!(Ordering::Sop.axis(), PartitionAxis::SubjectMajor);
        assert_eq!(Ordering::Pso.axis(), PartitionAxis::SubjectMajor);
        assert_eq!(Ordering::Pos.axis(), PartitionAxis::ObjectMajor);
        assert_eq!(Ordering::Osp.axis(), PartitionAxis::ObjectMajor);
        assert_eq!(Ordering::Ops.axis(), PartitionAxis::ObjectMajor);
    }

    #[test]
    fn all_lists_every_ordering_once() {
        assert_eq!(Ordering::ALL.len(), 6);
        for ord in Ordering::ALL {
            assert_eq!(Ordering::ALL.iter().filter(|o| **o == ord).count(), 1);
        }
    }
}
