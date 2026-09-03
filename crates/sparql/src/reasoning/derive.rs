//! The materializer — SPEC-29 D3/D4/D5/D7.
//!
//! One pass of [`ViewManager::run_until_clean`]:
//!
//! 1. If the spine changed, close it **once** into a template
//!    [`Engine`] and diff `closure(spine) − asserted(spine)` into
//!    [`SPINE_CLOSURE_GRAPH`].
//! 2. For each stale view: fork the template, extend the fork with the view's
//!    own source graph, and diff
//!    `closure(spine ∪ data) − closure(spine) − asserted(data)` into the
//!    view's inferred graph.
//!
//! Step 2 is D3's factoring made concrete: `lfp(T, S ∪ D) == lfp(T, lfp(T,S) ∪ D)`
//! holds for the monotone OWL 2 RL rule set, so the spine's closure is
//! computed once and reused by every view instead of once per view. Its two
//! stated invariant conditions are both live: `owl:sameAs` is materialized in
//! full with no representative canonicalization of stored triples (pinned by
//! `crates/owlrl/tests/spine_factoring.rs::sameas_across_the_split`), and an
//! inconsistent view sets a per-view flag rather than halting the store.
//!
//! Nothing here writes a source graph, which is D5: reading a source graph
//! back returns exactly the quads written to it.

use std::collections::BTreeSet;
use std::time::Instant;

use horndb_config::Reasoning;
use horndb_owlrl::Engine;

use super::{ViewCatalog, ViewSource, SPINE_CLOSURE_GRAPH, VIEWS_GRAPH};
use crate::exec::horn::{lexical_to_oxrdf, oxrdf_to_algebra, HornBackend};
use crate::exec::{AlgebraQuad, Store};
use crate::Result;

/// A lexical triple in the `Engine::materialized_triples()` convention.
type LexTriple = (String, String, String);

/// Owns the spine template and the view catalog, and drives derivation.
///
/// Single-threaded by construction: it borrows the backend mutably for the
/// duration of a pass. The server runs one of these on a background thread
/// (SPEC-29 P1's honest cost — P2 makes fan-out incremental).
pub struct ViewManager {
    cfg: Reasoning,
    catalog: ViewCatalog,
    /// The spine closed once, ready to [`Engine::fork`]. `None` until the
    /// first pass builds it.
    spine_engine: Option<Engine>,
    /// `closure(spine)` — subtracted from every view's closure so a view's
    /// inferred graph holds only what its *own* data adds (D3).
    spine_materialized: BTreeSet<LexTriple>,
}

impl ViewManager {
    pub fn new(cfg: &Reasoning) -> Self {
        Self {
            cfg: cfg.clone(),
            catalog: ViewCatalog::new(cfg),
            spine_engine: None,
            spine_materialized: BTreeSet::new(),
        }
    }

    pub fn catalog(&self) -> &ViewCatalog {
        &self.catalog
    }

    /// Fold the graphs a write touched into the catalog (SPEC-29 D7). Call
    /// after any batch of updates; it drains the backend's touched set.
    pub fn observe(&mut self, backend: &mut HornBackend) {
        let touched = backend.take_touched_graphs();
        self.catalog.route(&touched);
    }

    /// Derive until nothing is stale, and return how many views were derived.
    ///
    /// Disabled reasoning is a no-op: no engine runs, no reserved graph is
    /// written, and the D6 visibility list stays empty — so a store with
    /// `reasoning.enabled = false` is byte-for-byte the store it was before
    /// SPEC-29.
    pub fn run_until_clean(&mut self, backend: &mut HornBackend) -> Result<usize> {
        if !self.cfg.enabled {
            return Ok(0);
        }
        self.observe(backend);

        // Membership is rebuilt from the store, not recovered from disk: a
        // graph bulk-loaded below the write funnel is found here, and a
        // restart re-derives rather than trusting stale state. See the
        // module doc's SPEC-30 seam note.
        let graphs = backend.graphs();
        let default_non_empty = !backend.scan_graph_lexical(None)?.is_empty();
        for gone in self.catalog.refresh_membership(&graphs, default_non_empty) {
            self.clear_derived(backend, &gone.inferred_graph)?;
        }

        if self.catalog.spine_stale() {
            self.build_spine(backend, &graphs)?;
        }

        let mut derived = 0usize;
        while let Some(src) = self.catalog.next_dirty() {
            self.derive_view(backend, &src)?;
            derived += 1;
        }

        self.publish(backend)?;
        Ok(derived)
    }

    /// Close the spine once and publish `closure(spine) − asserted(spine)`.
    fn build_spine(&mut self, backend: &mut HornBackend, graphs: &[String]) -> Result<()> {
        let started = Instant::now();

        // `views.include_spine = false` means views reason over their own
        // graph alone: an empty spine, not a special code path.
        let asserted: BTreeSet<LexTriple> = if self.cfg.views.include_spine {
            let mut out = BTreeSet::new();
            for g in graphs.iter().filter(|g| self.catalog.is_spine_graph(g)) {
                out.extend(backend.scan_graph_lexical(Some(g.clone()))?);
            }
            out
        } else {
            BTreeSet::new()
        };

        let mut engine = Engine::new();
        engine
            .load_base(asserted.iter().cloned())
            .map_err(|e| crate::SparqlError::Executor(format!("spine closure: {e}")))?;
        self.spine_materialized = engine
            .materialized_triples()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let want: BTreeSet<LexTriple> = self
            .spine_materialized
            .difference(&asserted)
            .cloned()
            .collect();
        self.write_graph(backend, SPINE_CLOSURE_GRAPH, &want)?;

        self.spine_engine = Some(engine);
        self.catalog.mark_spine_fresh();

        let m = horndb_metrics::metrics();
        m.reasoning
            .spine_build_duration_seconds
            .observe(started.elapsed().as_secs_f64());
        m.reasoning
            .spine_version
            .set(self.catalog.spine_version() as i64);
        Ok(())
    }

    /// Derive one view: fork the closed spine, extend with the view's own
    /// graph, and publish what that addition — and only that addition —
    /// derives.
    fn derive_view(&mut self, backend: &mut HornBackend, src: &ViewSource) -> Result<()> {
        let started = Instant::now();

        let asserted: BTreeSet<LexTriple> = backend
            .scan_graph_lexical(src.graph_name())?
            .into_iter()
            .collect();

        let mut engine = match &self.spine_engine {
            Some(e) => e.fork(),
            // Only reachable if `build_spine` was skipped, which
            // `run_until_clean` does not do; fail loudly rather than
            // silently derive without a spine.
            None => {
                return Err(crate::SparqlError::Executor(
                    "derive_view before the spine template was built".into(),
                ))
            }
        };
        engine
            .extend(asserted.iter().cloned())
            .map_err(|e| crate::SparqlError::Executor(format!("view closure: {e}")))?;

        let materialized: BTreeSet<LexTriple> = engine
            .materialized_triples()
            .unwrap_or_default()
            .into_iter()
            .collect();
        // What this view adds beyond the spine's closure and its own asserted
        // data. Subtracting `spine_materialized` is what stops every view
        // replicating the vocabulary's closure into its own graph.
        let want: BTreeSet<LexTriple> = materialized
            .difference(&self.spine_materialized)
            .filter(|t| !asserted.contains(*t))
            .cloned()
            .collect();

        let inferred_graph = src.inferred_graph();
        self.write_graph(backend, &inferred_graph, &want)?;

        // D3 condition 2: inconsistency is a per-view flag, not a store-wide
        // halt. A view that cannot answer the question is treated as
        // inconsistent, which is the conservative reading.
        let consistent = engine.is_consistent().unwrap_or(false);
        self.catalog.mark_derived(src, consistent);

        let m = horndb_metrics::metrics();
        m.reasoning.view_derivations.inc();
        m.reasoning
            .derivation_duration_seconds
            .observe(started.elapsed().as_secs_f64());
        m.reasoning
            .views_dirty
            .set(self.catalog.dirty_count() as i64);
        Ok(())
    }

    /// Republish the catalog graph and refresh the D6 visibility list.
    fn publish(&mut self, backend: &mut HornBackend) -> Result<()> {
        let want: BTreeSet<LexTriple> = self.catalog.catalog_triples().into_iter().collect();
        self.write_graph(backend, VIEWS_GRAPH, &want)?;

        // SPEC-29 D6. Applied here rather than per query from `AppState`:
        // SPEC-26 phase 3 hot reload has not landed, so the flag cannot
        // change under a running server anyway.
        backend.set_visible_inferred(if self.cfg.default_dataset_includes_inferred {
            self.catalog.visible_inferred()
        } else {
            BTreeSet::new()
        });
        Ok(())
    }

    fn clear_derived(&self, backend: &mut HornBackend, graph: &str) -> Result<()> {
        self.write_graph(backend, graph, &BTreeSet::new())
    }

    /// Make `graph` hold exactly `want`, as one idempotent
    /// [`Store::apply_quads`] batch. Re-running with an unchanged `want`
    /// writes nothing and therefore marks nothing dirty — which is what lets
    /// a replayed update batch, or a restart mid-fan-out, converge instead of
    /// looping.
    fn write_graph(
        &self,
        backend: &mut HornBackend,
        graph: &str,
        want: &BTreeSet<LexTriple>,
    ) -> Result<()> {
        let current: BTreeSet<LexTriple> = backend
            .scan_graph_lexical(Some(graph.to_string()))?
            .into_iter()
            .collect();
        if current == *want {
            return Ok(());
        }
        let quad = |t: &LexTriple| -> AlgebraQuad {
            (
                Some(graph.to_string()),
                oxrdf_to_algebra(&lexical_to_oxrdf(&t.0)),
                oxrdf_to_algebra(&lexical_to_oxrdf(&t.1)),
                oxrdf_to_algebra(&lexical_to_oxrdf(&t.2)),
            )
        };
        let dels: Vec<AlgebraQuad> = current.difference(want).map(quad).collect();
        let adds: Vec<AlgebraQuad> = want.difference(&current).map(quad).collect();
        backend.apply_quads(dels, adds)?;
        // Our own writes must never dirty a view; drain them so the next
        // `observe` sees only real user writes.
        let touched = backend.take_touched_graphs();
        debug_assert!(
            touched.iter().all(|g| g.as_deref() == Some(graph)),
            "derivation wrote outside {graph}: {touched:?}"
        );
        Ok(())
    }
}
