//! SPEC-30 P1 — the applied-position slot: a change-feed consumer's
//! durability contract, stored as quads in the reserved graph
//! `https://horndb.io/graph/feed` (§S2).
//!
//! Not `server`-feature-gated: `crate::update::apply_update_with_feed` calls
//! into this module regardless of whether the HTTP server is compiled in, so
//! a non-server build (e.g. an embedded caller driving `apply_update`
//! directly) still gets the slot.
//!
//! **Architecture note (a documented simplification of the plan's literal
//! wording):** `docs/plans/PLAN-30-01-applied-position-slot.md` describes
//! appending the slot's retract/insert quads to the request's *final
//! operation's own* `apply_quads` call. This module instead issues one
//! separate `apply_quads` call for the slot, strictly *after* every
//! operation in the request has committed (see
//! `update::apply_update_with_feed`). Both give the same D1/D5 guarantee on
//! a backend with no per-call durability boundary (today's fully in-memory
//! store): the slot never becomes visible ahead of the data it describes,
//! because it is applied only once every operation already committed
//! without error, and a mid-request failure short-circuits before the slot
//! call ever runs. The separate-call design needed no changes to
//! `apply_op`'s per-operation-kind branches, so it is the smaller diff for
//! the same tested contract; P3/P4 (WAL/checkpoint riding, out of this
//! plan's scope) may need to revisit this once there is a real per-call
//! durability point to reason about.

use crate::algebra::Term;
use crate::error::Result;
use crate::exec::{AlgebraQuad, AlgebraTriple, Store};
use spargebra::algebra::GraphTarget;
use spargebra::term::NamedNode;

/// The reserved graph the slot lives in (SPEC-30 §S2).
pub const FEED_GRAPH: &str = "https://horndb.io/graph/feed";
/// The slot's one subject.
pub const FEED_SLOT_SUBJECT: &str = "https://horndb.io/graph/feed#slot";

const PRED_ID: &str = "https://horndb.io/ns/feed#id";
const PRED_GENERATION: &str = "https://horndb.io/ns/feed#generation";
const PRED_POSITION: &str = "https://horndb.io/ns/feed#position";
const PRED_ADVANCED_AT: &str = "https://horndb.io/ns/feed#advancedAt";

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// A consumer-supplied feed id + opaque position token, carried on a request
/// (SPEC-30 §S2, D2: `position` is never parsed, compared, or ordered —
/// stored and returned verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPosition {
    pub id: String,
    pub position: String,
}

/// The slot's contents as read back from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    id: String,
    generation: i64,
    position: String,
    advanced_at: String,
}

/// Encode a plain string as an N-Triples `STRING_LITERAL_QUOTE`, escaping the
/// characters the grammar requires (`\`, `"`, newline, CR, tab). The
/// position token is opaque (D2) and may contain any of these.
fn escape_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Reverse of [`escape_literal`]'s body (the text between the quotes).
fn unescape_literal_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split an N-Triples literal lexical form (`"body"` or
/// `"body"^^<datatype>`/`"body"@lang`) into its quoted body, honouring `\`
/// escapes. `None` if `lex` does not start with `"` or the closing quote is
/// missing.
fn literal_body(lex: &str) -> Option<&str> {
    if !lex.starts_with('"') {
        return None;
    }
    let bytes = lex.as_bytes();
    let mut i = 1;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if escaped {
            escaped = false;
        } else if c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            return Some(&lex[1..i]);
        }
        i += 1;
    }
    None
}

/// A plain (no datatype/language) literal term.
fn plain_literal(value: &str) -> Term {
    Term::Literal(escape_literal(value))
}

/// A typed literal term.
fn typed_literal(value: &str, datatype_iri: &str) -> Term {
    Term::Literal(format!("{}^^<{datatype_iri}>", escape_literal(value)))
}

/// Decode a literal `Term`'s value back to a plain string. `None` for
/// anything that is not a literal, or a literal this module cannot parse.
fn decode_literal(t: &Term) -> Option<String> {
    match t {
        Term::Literal(lex) => literal_body(lex).map(unescape_literal_body),
        _ => None,
    }
}

/// The four quads a [`Slot`] encodes to, in the feed graph.
fn slot_quads(slot: &Slot) -> Vec<AlgebraQuad> {
    let g = Some(FEED_GRAPH.to_owned());
    let subj = || Term::Iri(FEED_SLOT_SUBJECT.to_owned());
    vec![
        (
            g.clone(),
            subj(),
            Term::Iri(PRED_ID.to_owned()),
            plain_literal(&slot.id),
        ),
        (
            g.clone(),
            subj(),
            Term::Iri(PRED_GENERATION.to_owned()),
            typed_literal(&slot.generation.to_string(), XSD_INTEGER),
        ),
        (
            g.clone(),
            subj(),
            Term::Iri(PRED_POSITION.to_owned()),
            plain_literal(&slot.position),
        ),
        (
            g,
            subj(),
            Term::Iri(PRED_ADVANCED_AT.to_owned()),
            typed_literal(&slot.advanced_at, XSD_DATETIME),
        ),
    ]
}

/// Read the slot currently in the feed graph, `None` if it holds no (or an
/// incomplete — `id`/`position` are the two load-bearing fields) slot.
fn read_slot<B: Store>(store: &B) -> Result<Option<Slot>> {
    let target = GraphTarget::NamedNode(NamedNode::new_unchecked(FEED_GRAPH));
    let triples: Vec<AlgebraTriple> = store.scan_graph_quads(&target)?;
    if triples.is_empty() {
        return Ok(None);
    }
    let mut id = None;
    let mut generation = None;
    let mut position = None;
    let mut advanced_at = None;
    for (_s, p, o) in &triples {
        let Term::Iri(pred) = p else { continue };
        match pred.as_str() {
            PRED_ID => id = decode_literal(o),
            PRED_GENERATION => generation = decode_literal(o).and_then(|v| v.parse::<i64>().ok()),
            PRED_POSITION => position = decode_literal(o),
            PRED_ADVANCED_AT => advanced_at = decode_literal(o),
            _ => {}
        }
    }
    match (id, position) {
        (Some(id), Some(position)) => Ok(Some(Slot {
            id,
            generation: generation.unwrap_or(0),
            position,
            advanced_at: advanced_at.unwrap_or_default(),
        })),
        _ => Ok(None),
    }
}

/// `dels`/`adds` for replacing `old` (if any) with `new` in one `apply_quads`
/// call — `apply_quads`'s dels-before-adds rule (SPEC-28 S6) makes an
/// identical replayed advance a clean overwrite.
fn slot_delta(old: Option<&Slot>, new: &Slot) -> (Vec<AlgebraQuad>, Vec<AlgebraQuad>) {
    let dels = old.map(slot_quads).unwrap_or_default();
    let adds = slot_quads(new);
    (dels, adds)
}

/// `OffsetDateTime::now_utc()` formatted as `xsd:dateTime` (RFC 3339, which
/// `xsd:dateTime` accepts).
fn now_xsd_datetime() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// SPEC-30 D6: refuse a request whose feed id conflicts with a non-empty
/// slot's id. An empty slot adopts the first id it is given. Read-only —
/// call this before any operation in the request applies (the preflight
/// half of D6; the "at startup" half is the consumer's own reconciliation).
pub fn check_feed_id<B: Store>(store: &B, feed: &FeedPosition) -> Result<()> {
    if let Some(slot) = read_slot(store)? {
        if slot.id != feed.id {
            return Err(crate::error::SparqlError::FeedIdMismatch {
                slot: slot.id,
                request: feed.id.clone(),
            });
        }
    }
    Ok(())
}

/// Advance the slot to `feed`'s id/position, generation pinned at 0 (P1 —
/// the rebuild reset that increments it is P2, out of this plan's scope).
/// One `apply_quads` call: retract whatever slot quads exist today, insert
/// the new ones. Call this only after every operation in the request has
/// committed (S5) — see `update::apply_update_with_feed`.
pub fn advance_slot<B: Store>(store: &mut B, feed: &FeedPosition) -> Result<()> {
    let old = read_slot(store)?;
    let new = Slot {
        id: feed.id.clone(),
        generation: 0,
        position: feed.position.clone(),
        advanced_at: now_xsd_datetime(),
    };
    let (dels, adds) = slot_delta(old.as_ref(), &new);
    store.apply_quads(dels, adds)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_round_trips_special_characters() {
        for s in [
            "plain",
            "with\"quote",
            "with\\backslash",
            "with\nnewline",
            "",
        ] {
            let lex = plain_literal(s);
            assert_eq!(decode_literal(&lex).as_deref(), Some(s));
        }
    }

    #[test]
    fn slot_delta_replaces_old_with_new() {
        let old = Slot {
            id: "a".into(),
            generation: 0,
            position: "1".into(),
            advanced_at: "2026-01-01T00:00:00Z".into(),
        };
        let new = Slot {
            id: "a".into(),
            generation: 0,
            position: "2".into(),
            advanced_at: "2026-01-02T00:00:00Z".into(),
        };
        let (dels, adds) = slot_delta(Some(&old), &new);
        assert_eq!(dels.len(), 4);
        assert_eq!(adds.len(), 4);
        assert_ne!(dels, adds);
    }

    #[test]
    fn slot_delta_with_no_prior_slot_has_no_deletions() {
        let new = Slot {
            id: "a".into(),
            generation: 0,
            position: "1".into(),
            advanced_at: "2026-01-01T00:00:00Z".into(),
        };
        let (dels, adds) = slot_delta(None, &new);
        assert!(dels.is_empty());
        assert_eq!(adds.len(), 4);
    }
}
