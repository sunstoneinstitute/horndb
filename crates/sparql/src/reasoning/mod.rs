//! Named-graph reasoning views — SPEC-29 P1.
//!
//! What this delivers, in one paragraph: reasoning scope stops being "whatever
//! got loaded" and becomes a **declared view** (D1) — a shared vocabulary
//! *spine* plus exactly one data graph (D2). The spine closes **once** into a
//! reusable template engine (D3); each view forks that template, extends it
//! with its own graph, and writes only what it derives *beyond* the spine
//! closure into its own **inferred graph** under the reserved
//! `https://horndb.io/graph/` namespace (D4). A source graph is therefore
//! never written by reasoning, so reading one back returns exactly the quads
//! that were written to it (D5 — the invariant that keeps a whole-graph `PUT`
//! diff from deleting inferences the client never sent).
//!
//! Input deltas are quads and routing is per view (D7): the backend reports
//! which graphs a write actually changed
//! ([`HornBackend::take_touched_graphs`](crate::exec::horn::HornBackend::take_touched_graphs)),
//! a touched data graph marks its own view stale, and a touched spine graph
//! bumps the spine version and marks every view stale. Config is the
//! server-scoped `[reasoning]` section (D9), never a per-query override.
//!
//! **Batch engines, not circuits.** P1's re-derivation runs on the existing
//! batch [`Engine`](horndb_owlrl::Engine) via `load_base`/`fork`/`extend`.
//! SPEC-29 P2 replaces the re-derive step with the delta-incremental path;
//! the view model, catalog, routing and invariants here are unchanged by that
//! swap.
//!
//! **Not in P1** (deliberately, per the spec's phasing): incremental spine
//! fan-out and the `fanout.*` config keys (P2), provenance graph attribution
//! (P3), virtual views / `views.output = "none"` (P4), and migrating the
//! `--materialize` CLI path onto views.
//!
//! ## Where SPEC-30's recovery hook attaches
//!
//! SPEC-30 P1 (applied-position slot + durability contract,
//! <https://github.com/sunstoneinstitute/horndb/issues/270>) has not landed,
//! so P1 invents no durability format. Instead, derived state is treated as
//! disposable: [`ViewManager::new`] starts with an empty catalog and
//! [`ViewManager::run_until_clean`] rebuilds membership from the store's
//! current graph list on every pass, marking every previously unseen view
//! dirty. Because a derivation is an idempotent diff against the inferred
//! graph's current contents, a restart mid-fan-out converges to the same state
//! without recovering anything. When SPEC-30 lands, the seam is
//! [`ViewManager::run_until_clean`]'s per-view loop: after each view's
//! `apply_quads` batch commits, record the applied position alongside the
//! catalog quads that same batch already writes.

pub mod catalog;
pub mod derive;

pub use catalog::{ViewCatalog, ViewState};
pub use derive::ViewManager;

use horndb_config::{Reasoning, ViewOutput, ViewSelect};

use crate::exec::scope::RESERVED_GRAPH_PREFIX;

/// Per-view inferred graphs are minted under this prefix (SPEC-29 D4).
pub const INFERRED_GRAPH_PREFIX: &str = "https://horndb.io/graph/inferred/";

/// The one shared graph holding the spine's derived-beyond-asserted triples
/// (SPEC-29 D3). Views never replicate these.
pub const SPINE_CLOSURE_GRAPH: &str = "https://horndb.io/graph/spine-closure";

/// The view catalog graph (SPEC-29 D4): one node per view, so an operator can
/// read staleness with a query instead of guessing at the IRI encoding.
pub const VIEWS_GRAPH: &str = "https://horndb.io/graph/views";

/// The catalog vocabulary. Minimal on purpose — SPEC-27's `hprov:` terms and
/// SPEC-29 D8's `hprov:view` join arrive with P3.
pub const NS: &str = "https://horndb.io/ns/reasoning#";

/// The inferred-graph path reserved for the default graph, which has no IRI
/// of its own.
const DEFAULT_SOURCE_SEGMENT: &str = "default";

/// Named sources live one path segment deeper. Their encoded segment can
/// never contain a `/` (it is not in the unreserved set), so no named source
/// can collide with [`DEFAULT_SOURCE_SEGMENT`] however it is spelled.
const NAMED_SOURCE_SEGMENT: &str = "g/";

/// The source graph one reasoning view reasons over, on top of the spine.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ViewSource {
    /// The default-graph sentinel (the degenerate single-view case).
    Default,
    /// A named graph, by IRI.
    Named(String),
}

impl ViewSource {
    /// This view's graph in the [`crate::exec::GraphName`] convention
    /// (`None` = the default graph).
    pub fn graph_name(&self) -> crate::exec::GraphName {
        match self {
            ViewSource::Default => None,
            ViewSource::Named(iri) => Some(iri.clone()),
        }
    }

    /// Mint this view's inferred graph IRI (SPEC-29 D4). Deterministic from
    /// the source IRI, so two HornDB processes over the same data agree, and
    /// exactly reversible by [`source_of_inferred_graph`], so a collision is
    /// impossible.
    pub fn inferred_graph(&self) -> String {
        let segment = match self {
            ViewSource::Default => DEFAULT_SOURCE_SEGMENT.to_string(),
            ViewSource::Named(iri) => {
                format!("{NAMED_SOURCE_SEGMENT}{}", percent_encode_segment(iri))
            }
        };
        format!("{INFERRED_GRAPH_PREFIX}{segment}")
    }
}

/// Percent-encode every byte outside RFC 3986's *unreserved* set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`). Deliberately more aggressive
/// than a URI-path escape: the source IRI is carried as one opaque segment,
/// so nothing in it may be mistaken for structure.
pub fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Exact inverse of [`percent_encode_segment`]. `None` if `s` is not a
/// well-formed encoding (a stray `%`, a bad hex pair, or bytes that are not
/// valid UTF-8).
pub fn percent_decode_segment(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Recover the source graph an inferred graph IRI was minted from, or `None`
/// if `iri` is not one of ours.
pub fn source_of_inferred_graph(iri: &str) -> Option<ViewSource> {
    let segment = iri.strip_prefix(INFERRED_GRAPH_PREFIX)?;
    if segment == DEFAULT_SOURCE_SEGMENT {
        return Some(ViewSource::Default);
    }
    percent_decode_segment(segment.strip_prefix(NAMED_SOURCE_SEGMENT)?).map(ViewSource::Named)
}

/// True if `iri` is a graph HornDB's reasoning writes, rather than one it
/// reasons over.
pub fn is_reasoning_output(iri: &str) -> bool {
    iri.starts_with(RESERVED_GRAPH_PREFIX)
}

/// Does `pattern` (an IRI or an IRI prefix) select `iri`?
pub fn pattern_matches(pattern: &str, iri: &str) -> bool {
    iri.starts_with(pattern)
}

/// Domain validation of `[reasoning]` beyond what serde already rejects
/// (SPEC-29 D9). Returns the warnings to log on success; the `Err` string is
/// a fatal startup message naming the offending key, following
/// `serve.rs`'s `[simd].max_isa` pattern.
///
/// Lives here rather than in `horndb-config` so that crate stays serde-only,
/// and so the rules are unit-testable without starting a server.
pub fn validate(cfg: &Reasoning) -> std::result::Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    // A pattern that reaches into the reserved namespace would put HornDB's
    // own output back into its own input.
    for (key, pattern) in cfg.spine.iter().map(|p| ("[reasoning].spine", p)).chain(
        select_patterns(cfg)
            .iter()
            .map(|p| ("[reasoning].views.select", p)),
    ) {
        if pattern_matches(pattern, RESERVED_GRAPH_PREFIX)
            || pattern_matches(RESERVED_GRAPH_PREFIX, pattern)
        {
            return Err(format!(
                "{key} pattern {pattern:?} matches the reserved namespace \
                 {RESERVED_GRAPH_PREFIX:?}, which HornDB writes its own \
                 derived graphs into"
            ));
        }
    }

    // A graph in both the spine and the view selection would be reasoned over
    // twice, with the two answers disagreeing about D3's factoring.
    for spine in &cfg.spine {
        for select in select_patterns(cfg) {
            if pattern_matches(spine, select) || pattern_matches(select, spine) {
                return Err(format!(
                    "[reasoning].spine pattern {spine:?} and \
                     [reasoning].views.select pattern {select:?} select the \
                     same graphs; a graph is either spine or a view source, \
                     never both"
                ));
            }
        }
    }

    if cfg.views.output == ViewOutput::None {
        return Err(
            "[reasoning].views.output = \"none\" (virtual, backward-chained views) \
             is SPEC-29 P4 and not implemented; use \"graph\""
                .to_string(),
        );
    }

    if cfg.enabled && cfg.spine.is_empty() {
        warnings.push(
            "[reasoning].enabled is set with an empty [reasoning].spine: every view \
             will derive only from its own graph, so no shared vocabulary axiom \
             (rdfs:subClassOf and friends) will fire. This is legal but rarely \
             intended."
                .to_string(),
        );
    }

    Ok(warnings)
}

/// The explicit `views.select` patterns, or nothing for the
/// `"all-except-spine"` default template.
pub(crate) fn select_patterns(cfg: &Reasoning) -> &[String] {
    match &cfg.views.select {
        ViewSelect::Keyword(_) => &[],
        ViewSelect::Patterns(p) => p.as_slice(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horndb_config::{ViewSelectKeyword, Views};

    /// SPEC-29 D4: minting is exactly reversible, so two distinct source
    /// graphs can never land on the same inferred graph. The nasty inputs
    /// are the ones that would break a naive escape: a `%` already in the
    /// IRI, path separators, a fragment, and non-ASCII.
    #[test]
    fn minting_roundtrips_and_is_injective() {
        let sources = [
            ViewSource::Default,
            ViewSource::Named("http://ex/g1".into()),
            ViewSource::Named("https://ex.org/a/b#frag".into()),
            ViewSource::Named("https://ex.org/100%25".into()),
            ViewSource::Named("https://ex.org/blåbær".into()),
            ViewSource::Named("https://ex.org/a?x=1&y=2".into()),
            // The one input that could collide with the default sentinel.
            ViewSource::Named("default".into()),
        ];
        let mut minted = std::collections::BTreeSet::new();
        for s in &sources {
            let g = s.inferred_graph();
            assert!(
                is_reasoning_output(&g),
                "{g} must be under the reserved namespace"
            );
            assert_eq!(
                source_of_inferred_graph(&g).as_ref(),
                Some(s),
                "{g} must decode back to its source"
            );
            assert!(minted.insert(g.clone()), "{g} was minted twice");
        }
        assert_eq!(minted.len(), sources.len());
    }

    #[test]
    fn decoding_rejects_malformed_segments() {
        assert_eq!(percent_decode_segment("%"), None);
        assert_eq!(percent_decode_segment("%ZZ"), None);
        assert_eq!(percent_decode_segment("%FF"), None, "not valid UTF-8");
        assert_eq!(source_of_inferred_graph("http://ex/not-ours"), None);
    }

    fn cfg(spine: &[&str], select: ViewSelect) -> Reasoning {
        Reasoning {
            enabled: true,
            spine: spine.iter().map(|s| s.to_string()).collect(),
            views: Views {
                select,
                ..Views::default()
            },
            default_dataset_includes_inferred: false,
            ..Reasoning::default()
        }
    }

    /// SPEC-29 D9's three validation rules, asserted through the function's
    /// return value rather than by scraping logs.
    #[test]
    fn validation_enforces_d9() {
        let all = ViewSelect::Keyword(ViewSelectKeyword::AllExceptSpine);

        // Reserved-namespace pattern: fatal, naming the key.
        let e = validate(&cfg(&["https://horndb.io/graph/"], all.clone())).unwrap_err();
        assert!(e.contains("[reasoning].spine"), "{e}");
        let e = validate(&cfg(
            &[],
            ViewSelect::Patterns(vec!["https://horndb.io/graph/inferred/".into()]),
        ))
        .unwrap_err();
        assert!(e.contains("[reasoning].views.select"), "{e}");

        // Overlap: fatal, naming both keys.
        let e = validate(&cfg(
            &["https://ex.org/vocab/"],
            ViewSelect::Patterns(vec!["https://ex.org/vocab/dcat".into()]),
        ))
        .unwrap_err();
        assert!(e.contains("[reasoning].spine"), "{e}");
        assert!(e.contains("[reasoning].views.select"), "{e}");

        // Disjoint patterns: fine, no warning.
        let ok = validate(&cfg(
            &["https://ex.org/vocab/"],
            ViewSelect::Patterns(vec!["https://ex.org/data/".into()]),
        ))
        .unwrap();
        assert!(ok.is_empty(), "{ok:?}");

        // Enabled with an empty spine: accepted, warned.
        let ok = validate(&cfg(&[], all.clone())).unwrap();
        assert_eq!(ok.len(), 1, "{ok:?}");
        assert!(ok[0].contains("spine"), "{ok:?}");

        // Disabled with an empty spine: not even a warning.
        let mut off = cfg(&[], all);
        off.enabled = false;
        assert!(validate(&off).unwrap().is_empty());

        // views.output = "none" is P4, and says so.
        let mut virtual_view = cfg(&["https://ex.org/vocab/"], ViewSelect::Patterns(vec![]));
        virtual_view.views.output = ViewOutput::None;
        let e = validate(&virtual_view).unwrap_err();
        assert!(e.contains("P4"), "{e}");
    }
}
