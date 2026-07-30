//! The graph scope a scan runs in (SPEC-28 S3).
//!
//! A plan-level [`GraphScope`] alone does not name a set of graphs: what the
//! *default graph* contains depends on the query's `FROM`/`FROM NAMED`
//! clause and, absent one, on the `default_graph` mode (D2). [`ScanScope`]
//! carries all three, and [`ScanScope::resolve`] folds them into the
//! backend-independent [`ResolvedScope`] each executor maps onto its own
//! storage.

use crate::algebra::{DatasetSpec, GraphSpec, Var};
use crate::error::SparqlError;
use crate::plan::GraphScope;
use crate::DefaultGraphMode;

/// IRI prefix reserved for HornDB-internal graphs (SPEC-27 F6, SPEC-29 D4).
/// Graphs under it are excluded from the no-dataset default graph in both
/// `default_graph` modes, and from `GRAPH ?g` enumeration; naming one
/// explicitly (`FROM`, `FROM NAMED`, ground `GRAPH <g>`) is the opt-in.
pub const RESERVED_GRAPH_PREFIX: &str = "https://horndb.io/graph/";

/// True if `iri` names a reserved (HornDB-internal) graph.
pub fn is_reserved_graph(iri: &str) -> bool {
    iri.starts_with(RESERVED_GRAPH_PREFIX)
}

const DEFAULT_GRAPH_SCOPE: GraphScope = GraphScope::DefaultGraph;
const NO_DATASET: DatasetSpec = DatasetSpec {
    default: None,
    named: None,
};
const NO_GRAPHS: &[String] = &[];

/// Everything an executor needs to turn one scan leaf into a set of graphs.
#[derive(Debug, Clone, Copy)]
pub struct ScanScope<'a> {
    /// The scan leaf's own scope, as lowering pushed it down.
    pub graph: &'a GraphScope,
    /// The query's `FROM`/`FROM NAMED` clause.
    pub dataset: &'a DatasetSpec,
    /// How to compose the default graph when `dataset` names nothing.
    pub mode: DefaultGraphMode,
}

impl ScanScope<'static> {
    /// A bare BGP in a query with no dataset clause, under the default
    /// (`union`) mode. The scope of every scan before SPEC-28 phase 3, and
    /// the convenient scope for tests and callers with no query context.
    pub const DEFAULT: ScanScope<'static> = ScanScope {
        graph: &DEFAULT_GRAPH_SCOPE,
        dataset: &NO_DATASET,
        mode: DefaultGraphMode::Union,
    };
}

impl<'a> ScanScope<'a> {
    pub fn new(graph: &'a GraphScope, dataset: &'a DatasetSpec, mode: DefaultGraphMode) -> Self {
        Self {
            graph,
            dataset,
            mode,
        }
    }

    /// A scope for *cardinality estimation only*: the leaf's own graph
    /// scope, with no dataset clause and the default mode.
    ///
    /// `EXPLAIN` has no dataset to hand and estimates never reach a result
    /// (`cardinality_estimate` returns `Option<usize>` into the planner,
    /// never a `Batch`), so widening the default graph here can only make an
    /// estimate coarser — which SPEC-28 S3 explicitly permits.
    pub fn estimating(graph: &'a GraphScope) -> Self {
        Self {
            graph,
            dataset: &NO_DATASET,
            mode: DefaultGraphMode::Union,
        }
    }

    /// Fold the scope, dataset and mode into the graph set to read.
    pub fn resolve(&self) -> ResolvedScope<'a> {
        match self.graph {
            GraphScope::DefaultGraph => match &self.dataset.default {
                // No `FROM`: the mode decides.
                None => match self.mode {
                    DefaultGraphMode::Union => ResolvedScope::DefaultUnion,
                    DefaultGraphMode::Strict => ResolvedScope::DefaultStrict,
                },
                // `FROM <g1> FROM <g2> …` — exactly those, term-level set
                // union. `Some(vec![])` (i.e. `FROM NAMED` with no `FROM`)
                // is the empty default graph (SPARQL 1.1 §13.2, D4).
                Some(list) => ResolvedScope::Union(list.as_slice()),
            },
            GraphScope::Named(GraphSpec::Iri(g)) => match &self.dataset.named {
                // No `FROM NAMED`: every graph is addressable by name. An
                // IRI naming no graph yields zero rows, not an error.
                None => ResolvedScope::OneGraph(g.as_str()),
                Some(list) if list.iter().any(|n| n == g) => ResolvedScope::OneGraph(g.as_str()),
                // Named, but outside the dataset's named set: zero rows.
                Some(_) => ResolvedScope::Union(NO_GRAPHS),
            },
            GraphScope::Named(GraphSpec::Var(v)) => ResolvedScope::PerGraph(v),
        }
    }
}

/// A [`ScanScope`] resolved to the graphs to read, named by IRI so both
/// backends can map it onto their own storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedScope<'a> {
    /// Every non-reserved graph, including the default-graph sentinel,
    /// as a **set** union of triples (D2 `union`).
    DefaultUnion,
    /// The default-graph sentinel alone (D2 `strict`).
    DefaultStrict,
    /// The set union of exactly these graphs. Empty = the empty graph.
    Union(&'a [String]),
    /// Exactly this graph.
    OneGraph(&'a str),
    /// `GRAPH ?g`: one scan per named graph, binding `?g` per row (D6).
    /// PLAN-28-03 Task 4 implements it; until then executors refuse it via
    /// [`per_graph_unsupported`] rather than answer over the wrong graphs.
    PerGraph(&'a Var),
}

/// The refusal every executor returns for [`ResolvedScope::PerGraph`] until
/// PLAN-28-03 Task 4 lands. A wrong answer is worse than a refusal
/// (SPEC-28 D1), and the variable form has no correct approximation.
pub fn per_graph_unsupported(var: &Var) -> SparqlError {
    SparqlError::UnsupportedAlgebra(format!(
        "GRAPH ?{} (the variable graph form) is not implemented yet \
         (SPEC-28 S3/D6, #266)",
        var.name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(iri: &str) -> GraphScope {
        GraphScope::Named(GraphSpec::Iri(iri.to_owned()))
    }

    #[test]
    fn no_dataset_follows_the_mode() {
        let ds = DatasetSpec::default();
        assert_eq!(
            ScanScope::new(&GraphScope::DefaultGraph, &ds, DefaultGraphMode::Union).resolve(),
            ResolvedScope::DefaultUnion
        );
        assert_eq!(
            ScanScope::new(&GraphScope::DefaultGraph, &ds, DefaultGraphMode::Strict).resolve(),
            ResolvedScope::DefaultStrict
        );
    }

    #[test]
    fn from_wins_over_the_mode() {
        let ds = DatasetSpec {
            default: Some(vec!["http://ex/g".into()]),
            named: Some(vec![]),
        };
        for mode in [DefaultGraphMode::Union, DefaultGraphMode::Strict] {
            match ScanScope::new(&GraphScope::DefaultGraph, &ds, mode).resolve() {
                ResolvedScope::Union(l) => assert_eq!(l, ["http://ex/g".to_owned()]),
                other => panic!("expected the FROM list, got {other:?}"),
            }
        }
    }

    /// `FROM NAMED <g>` with no `FROM` — the default graph is empty, not
    /// "everything" (SPARQL 1.1 §13.2, D4).
    #[test]
    fn from_named_only_yields_an_empty_default_graph() {
        let ds = DatasetSpec {
            default: Some(vec![]),
            named: Some(vec!["http://ex/g".into()]),
        };
        assert_eq!(
            ScanScope::new(&GraphScope::DefaultGraph, &ds, DefaultGraphMode::Union).resolve(),
            ResolvedScope::Union(&[])
        );
    }

    #[test]
    fn ground_graph_outside_the_named_set_is_empty() {
        let ds = DatasetSpec {
            default: Some(vec![]),
            named: Some(vec!["http://ex/g1".into()]),
        };
        let inside = named("http://ex/g1");
        let outside = named("http://ex/g2");
        assert_eq!(
            ScanScope::new(&inside, &ds, DefaultGraphMode::Union).resolve(),
            ResolvedScope::OneGraph("http://ex/g1")
        );
        assert_eq!(
            ScanScope::new(&outside, &ds, DefaultGraphMode::Union).resolve(),
            ResolvedScope::Union(&[])
        );
    }

    #[test]
    fn reserved_prefix_test() {
        assert!(is_reserved_graph("https://horndb.io/graph/inferred"));
        assert!(!is_reserved_graph("http://ex/g"));
    }
}
