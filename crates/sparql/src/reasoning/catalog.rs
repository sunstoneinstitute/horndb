//! The view catalog — SPEC-29 D1/D2/D4.
//!
//! Two representations of the same thing. [`ViewCatalog`] is the worker's
//! in-memory working state; the quads it emits into
//! [`VIEWS_GRAPH`](super::VIEWS_GRAPH) are its exhaust, so an operator can
//! read which views are stale, which are inconsistent, and which spine
//! version each closed against, with an ordinary query.
//!
//! The in-memory copy is never recovered from those quads. On startup the
//! catalog is empty and every view it discovers starts **dirty**; because a
//! derivation is an idempotent diff, a restart converges without recovering
//! anything (see the module doc's note on the SPEC-30 seam).

use std::collections::{BTreeMap, BTreeSet};

use horndb_config::Reasoning;

use super::{
    is_reasoning_output, pattern_matches, select_patterns, ViewSource, NS, SPINE_CLOSURE_GRAPH,
    VIEWS_GRAPH,
};
use crate::exec::GraphName;

/// One view's derived state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewState {
    /// This view's inferred graph (SPEC-29 D4), minted from the source IRI.
    pub inferred_graph: String,
    /// The spine version this view's contents were derived against. A view
    /// whose value is behind [`ViewCatalog::spine_version`] is stale — D3's
    /// "a stale view is detectable rather than silently mixed".
    pub derived_at_spine_version: u64,
    /// Set when the view needs re-deriving.
    pub dirty: bool,
    /// SPEC-29 D3 condition 2: a view that derives `owl:Nothing` membership
    /// surfaces it as a per-view flag rather than halting the store. An
    /// inconsistent *spine* sets it on every view at once, which is the
    /// honest reading.
    pub consistent: bool,
}

/// The declared views over this store, plus the spine version they close
/// against.
#[derive(Clone, Debug)]
pub struct ViewCatalog {
    spine_patterns: Vec<String>,
    select_patterns: Vec<String>,
    spine_version: u64,
    /// The spine graphs changed and the template must be rebuilt.
    spine_stale: bool,
    views: BTreeMap<ViewSource, ViewState>,
}

impl ViewCatalog {
    /// An empty catalog for `cfg`. Membership arrives via
    /// [`Self::refresh_membership`].
    pub fn new(cfg: &Reasoning) -> Self {
        Self {
            spine_patterns: cfg.spine.clone(),
            select_patterns: select_patterns(cfg).to_vec(),
            spine_version: 0,
            // The template has never been built, so it is stale by
            // definition — this is what makes a cold start derive everything.
            spine_stale: true,
            views: BTreeMap::new(),
        }
    }

    /// Is `iri` one of the shared vocabulary graphs?
    pub fn is_spine_graph(&self, iri: &str) -> bool {
        self.spine_patterns.iter().any(|p| pattern_matches(p, iri))
    }

    /// SPEC-29 D1/D2 membership: a named graph gets a view when it is not a
    /// spine graph, not reserved, and selected. With the default
    /// `"all-except-spine"` template every remaining graph is selected;
    /// explicit `views.select` patterns narrow it.
    pub fn selects(&self, iri: &str) -> bool {
        if self.is_spine_graph(iri) || is_reasoning_output(iri) {
            return false;
        }
        self.select_patterns.is_empty()
            || self.select_patterns.iter().any(|p| pattern_matches(p, iri))
    }

    /// Bring membership in line with the store's current graphs. `graphs` is
    /// every named graph holding at least one quad; `default_non_empty` says
    /// whether the default graph does — the degenerate single-view case gets
    /// a view only when there is something in it.
    ///
    /// New views arrive dirty. A view whose source graph has vanished is
    /// dropped and returned, so the caller can clear its inferred graph.
    pub fn refresh_membership(
        &mut self,
        graphs: &[String],
        default_non_empty: bool,
    ) -> Vec<ViewState> {
        let mut wanted: BTreeSet<ViewSource> = graphs
            .iter()
            .filter(|g| self.selects(g))
            .map(|g| ViewSource::Named(g.clone()))
            .collect();
        if default_non_empty {
            wanted.insert(ViewSource::Default);
        }

        for src in &wanted {
            self.views.entry(src.clone()).or_insert_with(|| ViewState {
                inferred_graph: src.inferred_graph(),
                // Deliberately behind `spine_version`, so a view discovered
                // for the first time can never look fresh.
                derived_at_spine_version: u64::MAX,
                dirty: true,
                consistent: true,
            });
        }

        let gone: Vec<ViewSource> = self
            .views
            .keys()
            .filter(|s| !wanted.contains(s))
            .cloned()
            .collect();
        gone.iter()
            .filter_map(|s| self.views.remove(s))
            .collect::<Vec<_>>()
    }

    /// SPEC-29 D7 routing. For each graph a write actually changed:
    /// a data graph marks its own view dirty and nothing else; a spine graph
    /// bumps the spine version and marks **every** view dirty (P1's honest
    /// cost — P2 makes this incremental); a reserved graph marks nothing,
    /// because those writes are our own derivations.
    pub fn route(&mut self, touched: &[GraphName]) {
        let mut spine_touched = false;
        for g in touched {
            match g {
                Some(iri) if is_reasoning_output(iri) => {}
                Some(iri) if self.is_spine_graph(iri) => spine_touched = true,
                other => {
                    let src = match other {
                        Some(iri) => ViewSource::Named(iri.clone()),
                        None => ViewSource::Default,
                    };
                    if let Some(v) = self.views.get_mut(&src) {
                        v.dirty = true;
                    }
                }
            }
        }
        if spine_touched {
            self.bump_spine();
        }
    }

    /// Mark the spine changed: every view that closed against the old version
    /// is now stale.
    pub fn bump_spine(&mut self) {
        self.spine_version += 1;
        self.spine_stale = true;
        for v in self.views.values_mut() {
            v.dirty = true;
        }
    }

    pub fn spine_version(&self) -> u64 {
        self.spine_version
    }

    pub fn spine_stale(&self) -> bool {
        self.spine_stale
    }

    pub fn mark_spine_fresh(&mut self) {
        self.spine_stale = false;
    }

    pub fn views(&self) -> &BTreeMap<ViewSource, ViewState> {
        &self.views
    }

    /// The next view needing work, in a stable order so a restart resumes
    /// deterministically.
    pub fn next_dirty(&self) -> Option<ViewSource> {
        self.views
            .iter()
            .find(|(_, v)| v.dirty)
            .map(|(s, _)| s.clone())
    }

    pub fn dirty_count(&self) -> usize {
        self.views.values().filter(|v| v.dirty).count()
    }

    /// Record the outcome of one derivation.
    pub fn mark_derived(&mut self, src: &ViewSource, consistent: bool) {
        let version = self.spine_version;
        if let Some(v) = self.views.get_mut(src) {
            v.dirty = false;
            v.consistent = consistent;
            v.derived_at_spine_version = version;
        }
    }

    /// SPEC-29 D6's opt-in list: **exactly** the per-view inferred graphs plus
    /// the spine-closure graph. Not the whole reserved prefix — the view
    /// catalog graph stays invisible to `GRAPH ?g`, which is acceptance 5's
    /// "and nothing else".
    pub fn visible_inferred(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .views
            .values()
            .map(|v| v.inferred_graph.clone())
            .collect();
        out.insert(SPINE_CLOSURE_GRAPH.to_string());
        out
    }

    /// The catalog as lexical triples for
    /// [`VIEWS_GRAPH`](super::VIEWS_GRAPH), in the same
    /// `Engine::materialized_triples()` convention every other triple in this
    /// module travels in — so the manager diffs them into place with exactly
    /// the same code path as a derived triple. The view node *is* the
    /// inferred graph IRI: already minted, already unique, and already the
    /// thing a client wants to query next.
    pub fn catalog_triples(&self) -> Vec<(String, String, String)> {
        let term = |local: &str| format!("{NS}{local}");
        let int = |n: u64| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
        let boolean = |b: bool| format!("\"{b}\"^^<http://www.w3.org/2001/XMLSchema#boolean>");
        let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string();

        let mut out = vec![
            (
                VIEWS_GRAPH.to_string(),
                term("spineVersion"),
                int(self.spine_version),
            ),
            (
                VIEWS_GRAPH.to_string(),
                term("spineClosureGraph"),
                SPINE_CLOSURE_GRAPH.to_string(),
            ),
        ];
        for (src, v) in &self.views {
            let node = v.inferred_graph.clone();
            out.push((node.clone(), rdf_type.clone(), term("View")));
            out.push((node.clone(), term("inferredGraph"), node.clone()));
            match src {
                ViewSource::Named(s) => out.push((node.clone(), term("source"), s.clone())),
                // The default graph has no IRI to point at, so the view is
                // typed instead of given a `source`.
                ViewSource::Default => {
                    out.push((node.clone(), rdf_type.clone(), term("DefaultGraphView")))
                }
            }
            out.push((
                node.clone(),
                term("derivedAtSpineVersion"),
                int(v.derived_at_spine_version),
            ));
            out.push((node.clone(), term("stale"), boolean(v.dirty)));
            out.push((node, term("consistent"), boolean(v.consistent)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horndb_config::{ViewSelect, Views};

    fn cfg(spine: &[&str], select: Option<&[&str]>) -> Reasoning {
        Reasoning {
            enabled: true,
            spine: spine.iter().map(|s| s.to_string()).collect(),
            views: Views {
                select: match select {
                    None => Views::default().select,
                    Some(p) => ViewSelect::Patterns(p.iter().map(|s| s.to_string()).collect()),
                },
                ..Views::default()
            },
            default_dataset_includes_inferred: false,
            ..Reasoning::default()
        }
    }

    /// SPEC-29 D1/D2's shipped default template: one view per graph that is
    /// neither spine nor reserved. The default graph joins only when it holds
    /// something.
    #[test]
    fn catalog_covers_non_spine_non_reserved_graphs() {
        let mut c = ViewCatalog::new(&cfg(&["https://ex.org/vocab/"], None));
        let graphs = [
            "https://ex.org/vocab/dcat".to_string(),
            "https://ex.org/data/a".to_string(),
            "https://ex.org/data/b".to_string(),
            "https://horndb.io/graph/inferred/x".to_string(),
            "https://horndb.io/graph/views".to_string(),
        ];
        assert!(c.refresh_membership(&graphs, false).is_empty());
        let sources: Vec<_> = c.views().keys().cloned().collect();
        assert_eq!(
            sources,
            vec![
                ViewSource::Named("https://ex.org/data/a".into()),
                ViewSource::Named("https://ex.org/data/b".into()),
            ]
        );
        assert_eq!(c.dirty_count(), 2, "new views arrive dirty");

        // The default graph joins when it is non-empty, and leaves again when
        // it empties — its state comes back so the caller can clear the
        // inferred graph.
        assert!(c.refresh_membership(&graphs, true).is_empty());
        assert!(c.views().contains_key(&ViewSource::Default));
        let gone = c.refresh_membership(&graphs, false);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].inferred_graph, ViewSource::Default.inferred_graph());

        // Explicit select patterns narrow it.
        let mut c = ViewCatalog::new(&cfg(
            &["https://ex.org/vocab/"],
            Some(&["https://ex.org/data/a"]),
        ));
        c.refresh_membership(&graphs, false);
        assert_eq!(
            c.views().keys().cloned().collect::<Vec<_>>(),
            vec![ViewSource::Named("https://ex.org/data/a".into())]
        );
    }

    /// SPEC-29 D7 routing: a data-graph write dirties exactly its own view; a
    /// spine write dirties every view and bumps the version; a write to our
    /// own output dirties nothing (or derivation would never terminate).
    #[test]
    fn routing_is_per_view_and_ignores_our_own_writes() {
        let mut c = ViewCatalog::new(&cfg(&["https://ex.org/vocab/"], None));
        c.refresh_membership(
            &[
                "https://ex.org/vocab/dcat".to_string(),
                "https://ex.org/data/a".to_string(),
                "https://ex.org/data/b".to_string(),
            ],
            false,
        );
        for src in c.views().keys().cloned().collect::<Vec<_>>() {
            c.mark_derived(&src, true);
        }
        c.mark_spine_fresh();
        assert_eq!(c.dirty_count(), 0);

        c.route(&[Some("https://ex.org/data/a".into())]);
        assert_eq!(c.dirty_count(), 1);
        assert_eq!(
            c.next_dirty(),
            Some(ViewSource::Named("https://ex.org/data/a".into()))
        );

        c.mark_derived(&ViewSource::Named("https://ex.org/data/a".into()), true);
        c.route(&[
            Some("https://horndb.io/graph/inferred/x".into()),
            Some("https://horndb.io/graph/views".into()),
            Some("https://horndb.io/graph/spine-closure".into()),
        ]);
        assert_eq!(c.dirty_count(), 0, "our own output must not dirty a view");

        let before = c.spine_version();
        c.route(&[Some("https://ex.org/vocab/dcat".into())]);
        assert_eq!(c.spine_version(), before + 1);
        assert_eq!(c.dirty_count(), 2, "a spine edit stales every view");
        assert!(c.spine_stale());
    }

    /// The D6 opt-in list is exactly the inferred graphs plus the spine
    /// closure — never the catalog graph.
    #[test]
    fn visible_inferred_excludes_the_catalog_graph() {
        let mut c = ViewCatalog::new(&cfg(&[], None));
        c.refresh_membership(&["https://ex.org/data/a".to_string()], false);
        let visible = c.visible_inferred();
        assert!(visible.contains(SPINE_CLOSURE_GRAPH));
        assert!(
            visible.contains(&ViewSource::Named("https://ex.org/data/a".into()).inferred_graph())
        );
        assert!(!visible.contains(VIEWS_GRAPH));
        assert_eq!(visible.len(), 2);
    }
}
