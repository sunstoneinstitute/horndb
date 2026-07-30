//! In-memory quad store. Stage 1 only.
//!
//! Triples are stored as `(String, String, String)` — i.e. all terms
//! are kept as their N-Triples lexical form — alongside the set of graphs
//! each one belongs to (SPEC-28 S3). This is intentionally simple; SPEC-02
//! introduces the real dictionary-encoded store.
//!
//! On top of the triple set we maintain a handful of hash indexes so a
//! triple pattern with one or more bound positions resolves to only the
//! matching triples instead of scanning the whole store. This keeps
//! multi-pattern BGP joins (every LDBC SPB aggregation query) tractable:
//! the per-pattern lookup is index-driven, turning the left-deep join
//! into an index-nested-loop join rather than an O(n×m) rescan. SPEC-03
//! (WCOJ) will replace this wholesale.

use crate::algebra::{Term, TriplePattern};
use crate::error::Result;
use crate::exec::scope::{
    is_reserved_graph, per_graph_needs_the_scan_loop, NamedGraph, ResolvedScope, ScanScope,
};
use crate::exec::{unify_one, Bindings, Executor, Slot, Store};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The graph a quad lives in: `None` is the default-graph sentinel (which
/// has no IRI), `Some(iri)` a named graph.
type GraphName = Option<String>;

/// In-memory quad store. Clone-on-write semantics — each
/// `MemStore` is independent.
///
/// The graph dimension is kept **beside** the triple table rather than in
/// its key: `triples` holds each distinct `(s, p, o)` once and `graphs[i]`
/// records which graphs hold it. That keeps every index (and the join) at
/// its pre-SPEC-28 shape, and makes a union scope's set semantics fall out
/// — one position is one row however many graphs hold the triple.
#[derive(Debug, Default, Clone)]
pub struct MemStore {
    /// Interned triples, addressed by position. Indexes hold positions
    /// into this vector.
    triples: Vec<(String, String, String)>,
    /// Which graphs hold `triples[i]`. Never empty (an emptied entry is
    /// removed from the store).
    graphs: Vec<HashSet<GraphName>>,
    /// Triple -> position, for O(1) lookup on insert/delete.
    pos: HashMap<(String, String, String), usize>,
    /// predicate -> triple positions.
    by_p: HashMap<String, Vec<usize>>,
    /// (predicate, object) -> triple positions.
    by_po: HashMap<(String, String), Vec<usize>>,
    /// (predicate, subject) -> triple positions.
    by_ps: HashMap<(String, String), Vec<usize>>,
    /// subject -> triple positions (for patterns that bind the subject
    /// but not the predicate, e.g. DESCRIBE forward scans).
    by_s: HashMap<String, Vec<usize>>,
}

impl MemStore {
    /// Insert a single default-graph triple from raw lexical-form strings.
    pub fn insert(&mut self, triple: (String, String, String)) {
        self.insert_quad(None, triple);
    }

    /// Insert a quad. `graph` is `None` for the default graph, `Some(iri)`
    /// for a named one. Inserting a triple already present in `graph` is a
    /// no-op; inserting it in a *second* graph records the extra membership
    /// without duplicating the triple.
    ///
    /// This is the seam SPEC-28 phase 3 needs to seed named graphs. The
    /// write trait (`exec::Store`) stays triple-shaped and default-graph
    /// scoped — the named-graph SPARQL Update path is phase 4 (#267).
    pub fn insert_quad(&mut self, graph: Option<&str>, triple: (String, String, String)) {
        let g = graph.map(str::to_owned);
        if let Some(&idx) = self.pos.get(&triple) {
            self.graphs[idx].insert(g);
            return;
        }
        let idx = self.triples.len();
        let (s, p, o) = &triple;
        self.by_p.entry(p.clone()).or_default().push(idx);
        self.by_po
            .entry((p.clone(), o.clone()))
            .or_default()
            .push(idx);
        self.by_ps
            .entry((p.clone(), s.clone()))
            .or_default()
            .push(idx);
        self.by_s.entry(s.clone()).or_default().push(idx);
        self.pos.insert(triple.clone(), idx);
        self.graphs.push(HashSet::from([g]));
        self.triples.push(triple);
    }

    /// Drop every quad and every index — a structural reset, independent of
    /// what the `Store` trait's `clear_all` means at any point in time.
    fn reset(&mut self) {
        self.triples.clear();
        self.graphs.clear();
        self.pos.clear();
        self.by_p.clear();
        self.by_po.clear();
        self.by_ps.clear();
        self.by_s.clear();
    }

    /// Number of **distinct triples** currently stored, across all graphs.
    /// A triple held by two graphs counts once. Stable; useful in tests.
    pub fn len(&self) -> usize {
        self.triples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }
    /// Iterate every stored triple in raw N-Triples lexical `(s, p, o)` form,
    /// in insertion order. Unlike [`Executor::scan_bgp`], this returns the
    /// *stored* strings verbatim (no kind reclassification), which lets a
    /// caller that tracks term kinds out-of-band — e.g. the rdflib-compatible
    /// Python binding (SPEC-10) — round-trip blank nodes and typed literals
    /// faithfully. SPEC-02's dictionary store will supersede this.
    pub fn iter_triples(&self) -> impl Iterator<Item = &(String, String, String)> {
        self.triples.iter()
    }
}

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => panic!("term_to_lex called on Var({})", v.name()),
        // RDF 1.2 triple-term patterns are gated by SparqlConfig::rdf12
        // at translation time; the planner only sees them on the rdf12
        // path, which the Stage-1 MemStore does not implement.
        Term::Triple(_) => panic!(
            "term_to_lex called on Term::Triple (rdf-12 patterns are unsupported by MemStore)"
        ),
    }
}

/// Resolve a pattern term against the current bindings to a *constant*
/// lexical value, if it has one. A constant pattern term (IRI / literal /
/// blank node) yields its lexical form; a variable already bound in
/// `row` yields the lexical form it is bound to; an unbound variable (or
/// triple term) yields `None`.
fn bound_lex(term: &Term, row: &Bindings) -> Option<String> {
    match term {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => Some(s.clone()),
        Term::Var(v) => row.get(v.name()).map(lex_of_bound),
        Term::Triple(_) => None,
    }
}

/// Lexical form of a term that was bound into a `Bindings` row. Bound
/// values always carry their lexical form in the inner string.
fn lex_of_bound(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
        Term::Triple(_) => String::new(),
    }
}

/// Which stored quads a scan may see, resolved once per scan from the
/// [`ScanScope`] (the `MemStore` analogue of `HornBackend`'s scoped
/// snapshot).
enum GraphFilter<'a> {
    /// The default-graph sentinel only (`strict`, and `FROM`-less strict).
    DefaultOnly,
    /// Every non-reserved graph, sentinel included (`union`).
    AnyNonReserved,
    /// Exactly these named graphs. Empty = the empty graph.
    Named(HashSet<&'a str>),
}

impl GraphFilter<'_> {
    /// True if a triple held by exactly `graphs` is visible here.
    fn admits(&self, graphs: &HashSet<GraphName>) -> bool {
        match self {
            GraphFilter::DefaultOnly => graphs.contains(&None),
            GraphFilter::AnyNonReserved => graphs
                .iter()
                .any(|g| g.as_deref().is_none_or(|iri| !is_reserved_graph(iri))),
            GraphFilter::Named(set) => graphs
                .iter()
                .any(|g| g.as_deref().is_some_and(|iri| set.contains(iri))),
        }
    }
}

fn graph_filter<'a>(scope: &ScanScope<'a>) -> Result<GraphFilter<'a>> {
    Ok(match scope.resolve() {
        ResolvedScope::DefaultStrict => GraphFilter::DefaultOnly,
        ResolvedScope::DefaultUnion => GraphFilter::AnyNonReserved,
        ResolvedScope::Union(list) => GraphFilter::Named(list.iter().map(String::as_str).collect()),
        ResolvedScope::OneGraph(g) => GraphFilter::Named(HashSet::from([g])),
        ResolvedScope::PerGraph { var, .. } => return Err(per_graph_needs_the_scan_loop(var)),
    })
}

impl Executor for MemStore {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        let filter = graph_filter(scope)?;
        // An empty group inside a ground `GRAPH <g>` matches only when `g` is
        // a graph of the dataset — see [`ScanScope::ground_graph`]. There is
        // no pattern to scan, so the test has to happen here or
        // `ASK { GRAPH <absent> {} }` answers `true` for every IRI.
        if patterns.is_empty()
            && scope.ground_graph().is_some()
            && !self.graphs.iter().any(|g| filter.admits(g))
        {
            return Ok(Box::new(std::iter::empty()));
        }
        // Left-deep, index-nested-loop join. For each pattern we resolve
        // the positions that are bound (either constants in the pattern
        // or variables already bound by an earlier pattern), pick the
        // most selective index covering those positions, and only unify
        // against the candidate triples it returns. With no bound
        // position we fall back to a full scan of that pattern — but in
        // a left-deep plan only the very first pattern is typically
        // fully unbound, and the SPB queries bind the predicate even
        // there.
        let mut current: Vec<Bindings> = vec![Bindings::new()];
        for pat in patterns {
            let mut next: Vec<Bindings> = Vec::new();
            for row in &current {
                for &idx in self.candidates(pat, row).iter() {
                    if !filter.admits(&self.graphs[idx]) {
                        continue;
                    }
                    let triple = &self.triples[idx];
                    if let Some(b) = unify_one(pat, triple, row) {
                        next.push(b);
                    }
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        Ok(Box::new(current.into_iter()))
    }

    /// The graphs `GRAPH ?g` enumerates, sorted by IRI. Walks the membership
    /// sets beside the triple table; the default graph (the `None` name) is
    /// never among them (D3). `?g` binds as a `Slot::Term` IRI, matching
    /// every other column this dictionary-less backend produces.
    fn named_graphs(&self, named: Option<&[String]>) -> Result<Vec<NamedGraph>> {
        let mut iris: BTreeSet<&str> = BTreeSet::new();
        for holders in &self.graphs {
            for g in holders.iter().filter_map(Option::as_deref) {
                let admitted = match named {
                    // No `FROM NAMED`: every non-reserved graph.
                    None => !is_reserved_graph(g),
                    // `FROM NAMED …`: exactly these, reserved included.
                    Some(list) => list.iter().any(|n| n == g),
                };
                if admitted {
                    iris.insert(g);
                }
            }
        }
        Ok(iris
            .into_iter()
            .map(|iri| NamedGraph {
                iri: iri.to_owned(),
                binding: Slot::Term(Term::Iri(iri.to_owned())),
            })
            .collect())
    }

    /// Scope-agnostic on purpose: the whole-store leaf-pattern count is a
    /// valid upper bound under every scope, and estimates never reach a
    /// result (SPEC-28 S3).
    fn cardinality_estimate(
        &self,
        patterns: &[TriplePattern],
        _scope: &ScanScope<'_>,
    ) -> Option<usize> {
        Some(self.estimate_bgp(patterns))
    }
}

impl MemStore {
    /// Cardinality estimate for `EXPLAIN`: the number of candidate
    /// triples for the *first* pattern, resolved through the same
    /// indexes `scan_bgp` uses, against an empty binding row. This is the
    /// leaf-pattern selectivity — an upper bound on the BGP output once
    /// later patterns join — which is what the Stage-1 plan printer wants
    /// (there is no cost model to chain selectivities through). An empty
    /// pattern list is the join identity: one row.
    fn estimate_bgp(&self, patterns: &[TriplePattern]) -> usize {
        match patterns.first() {
            None => 1,
            Some(first) => self.candidates(first, &Bindings::new()).len(),
        }
    }

    /// Candidate triple positions for `pat` given prior `row`. Picks the
    /// most selective available index for the bound positions; returns a
    /// borrowed slice when an index covers it, otherwise a full-range
    /// owned vector (only when nothing is bound).
    fn candidates(&self, pat: &TriplePattern, row: &Bindings) -> std::borrow::Cow<'_, [usize]> {
        use std::borrow::Cow;
        let s = bound_lex(&pat.subject, row);
        let p = bound_lex(&pat.predicate, row);
        let o = bound_lex(&pat.object, row);

        // Most selective first: a bound predicate plus a second bound
        // position. Then single-position indexes. Empty slice when a key
        // is absent from the index (no matching triples).
        let empty: &[usize] = &[];
        match (&s, &p, &o) {
            (_, Some(p), Some(o)) => Cow::Borrowed(
                self.by_po
                    .get(&(p.clone(), o.clone()))
                    .map_or(empty, Vec::as_slice),
            ),
            (Some(s), Some(p), _) => Cow::Borrowed(
                self.by_ps
                    .get(&(p.clone(), s.clone()))
                    .map_or(empty, Vec::as_slice),
            ),
            (_, Some(p), _) => Cow::Borrowed(self.by_p.get(p).map_or(empty, Vec::as_slice)),
            (Some(s), None, _) => Cow::Borrowed(self.by_s.get(s).map_or(empty, Vec::as_slice)),
            // Only the object is bound (no object-only index), or nothing
            // is bound: full scan of this pattern. The unbound-object,
            // unbound-predicate, unbound-subject case is the genuinely
            // unconstrained leading pattern.
            (None, None, _) => Cow::Owned((0..self.triples.len()).collect()),
        }
    }
}

impl Store for MemStore {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        self.insert((
            term_to_lex(&subject),
            term_to_lex(&predicate),
            term_to_lex(&object),
        ));
    }
    /// Retract the triple from the **default graph** only (the write trait
    /// is default-graph scoped until SPEC-28 phase 4). A triple that also
    /// lives in a named graph survives there.
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        let key = (
            term_to_lex(subject),
            term_to_lex(predicate),
            term_to_lex(object),
        );
        let Some(&idx) = self.pos.get(&key) else {
            return;
        };
        if !self.graphs[idx].remove(&None) {
            return; // not in the default graph — no-op
        }
        if !self.graphs[idx].is_empty() {
            return; // still held by a named graph; the triple stays
        }
        // Rebuild from the surviving quads. Deletion is rare (the server
        // loads once then serves read-only; `DELETE DATA` exists but is not
        // on a hot path), so a full rebuild keeps the index bookkeeping
        // trivially correct rather than juggling positional tombstones.
        let survivors: Vec<((String, String, String), HashSet<GraphName>)> =
            std::mem::take(&mut self.triples)
                .into_iter()
                .zip(std::mem::take(&mut self.graphs))
                .filter(|(t, _)| t != &key)
                .collect();
        // `reset`, not `clear_all`: the trait method's contract becomes
        // default-graph-only in SPEC-28 phase 4 (see its TODO), at which
        // point routing this rebuild through it would leave the indexes
        // populated and corrupt the store.
        self.reset();
        for (t, graphs) in survivors {
            // Re-insert once per graph the triple belongs to, moving the
            // strings on the last one so the common single-graph survivor
            // costs no clone at all.
            let mut rest = graphs.into_iter();
            let mut cur = rest.next();
            while let Some(g) = cur {
                cur = rest.next();
                match cur {
                    Some(_) => self.insert_quad(g.as_deref(), t.clone()),
                    None => {
                        self.insert_quad(g.as_deref(), t);
                        break;
                    }
                }
            }
        }
    }
    // TODO(#267): this sweeps every graph, named ones included. Once a
    // public write path can put data in a named graph via SPARQL Update,
    // `CLEAR DEFAULT`/`DROP DEFAULT` must stop routing here — same gap as
    // `HornBackend::clear_all` and the TODO in `crate::update::apply_clear_drop`.
    /// Whole-store wipe. Semantics are the `Store` trait's; internal callers
    /// that just want an empty store must use [`MemStore::reset`].
    fn clear_all(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;

    fn iri(s: &str) -> Term {
        Term::Iri(s.to_owned())
    }
    fn var(s: &str) -> Term {
        Term::Var(Var::new(s))
    }

    fn store() -> MemStore {
        let mut st = MemStore::default();
        // Two blog posts with titles, one with a body, plus noise.
        st.insert(("cw1".into(), "a".into(), "BlogPost".into()));
        st.insert(("cw1".into(), "title".into(), "\"First\"".into()));
        st.insert(("cw1".into(), "body".into(), "\"Hello\"".into()));
        st.insert(("cw2".into(), "a".into(), "BlogPost".into()));
        st.insert(("cw2".into(), "title".into(), "\"Second\"".into()));
        st.insert(("cw3".into(), "a".into(), "NewsItem".into()));
        st.insert(("cw3".into(), "title".into(), "\"Third\"".into()));
        st
    }

    fn pat(s: Term, p: Term, o: Term) -> TriplePattern {
        TriplePattern {
            subject: s,
            predicate: p,
            object: o,
        }
    }

    #[test]
    fn two_pattern_join_returns_correct_bindings() {
        let st = store();
        // ?cw a BlogPost . ?cw title ?t
        let patterns = vec![
            pat(var("cw"), iri("a"), iri("BlogPost")),
            pat(var("cw"), iri("title"), var("t")),
        ];
        let mut rows: Vec<(String, String)> = st
            .scan_bgp(&patterns, &ScanScope::DEFAULT)
            .unwrap()
            .map(|b| {
                (
                    lex_of_bound(b.get("cw").unwrap()),
                    lex_of_bound(b.get("t").unwrap()),
                )
            })
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("cw1".to_owned(), "\"First\"".to_owned()),
                ("cw2".to_owned(), "\"Second\"".to_owned()),
            ]
        );
        // cw3 is a NewsItem, must not appear.
    }

    #[test]
    fn three_pattern_join_narrows_to_single_row() {
        let st = store();
        // ?cw a BlogPost . ?cw title ?t . ?cw body ?b  -> only cw1
        let patterns = vec![
            pat(var("cw"), iri("a"), iri("BlogPost")),
            pat(var("cw"), iri("title"), var("t")),
            pat(var("cw"), iri("body"), var("b")),
        ];
        let rows: Vec<_> = st
            .scan_bgp(&patterns, &ScanScope::DEFAULT)
            .unwrap()
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(lex_of_bound(rows[0].get("cw").unwrap()), "cw1");
        assert_eq!(lex_of_bound(rows[0].get("t").unwrap()), "\"First\"");
        assert_eq!(lex_of_bound(rows[0].get("b").unwrap()), "\"Hello\"");
    }

    #[test]
    fn predicate_object_index_used_for_typed_pattern() {
        let st = store();
        // Single typed pattern hits the (p,o) index.
        let patterns = vec![pat(var("cw"), iri("a"), iri("BlogPost"))];
        let mut subs: Vec<String> = st
            .scan_bgp(&patterns, &ScanScope::DEFAULT)
            .unwrap()
            .map(|b| lex_of_bound(b.get("cw").unwrap()))
            .collect();
        subs.sort();
        assert_eq!(subs, vec!["cw1".to_owned(), "cw2".to_owned()]);
    }

    #[test]
    fn insert_dedup_and_delete_keeps_indexes_consistent() {
        let mut st = store();
        let before = st.len();
        st.insert(("cw1".into(), "a".into(), "BlogPost".into())); // dup
        assert_eq!(st.len(), before);
        st.delete_triple(
            &iri("cw2"),
            &iri("title"),
            &Term::Literal("\"Second\"".into()),
        );
        let patterns = vec![pat(var("cw"), iri("title"), var("t"))];
        let titles: Vec<String> = st
            .scan_bgp(&patterns, &ScanScope::DEFAULT)
            .unwrap()
            .map(|b| lex_of_bound(b.get("t").unwrap()))
            .collect();
        assert!(!titles.contains(&"\"Second\"".to_owned()));
        assert!(titles.contains(&"\"First\"".to_owned()));
    }
}
