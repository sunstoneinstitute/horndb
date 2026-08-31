//! 64-bit kind-tagged term IDs.
//!
//! See SPEC-02 F1/F2: high 4 bits encode `TermKind`, low 60 bits are payload.

use bytemuck::{Pod, Zeroable};

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Pod, Zeroable, Debug)]
pub struct TermId(pub u64);

#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum TermKind {
    Uri = 0,
    Blank = 1,
    PlainLiteral = 2,
    LangLiteral = 3,
    TypedLiteral = 4,
    InlineInt = 5,
    /// RDF 1.2 triple term (PR2 of the RDF 1.2 migration; tracked in
    /// SPEC-00 / TASKS.md). The payload is a dictionary index pointing
    /// at a recursively-interned `oxrdf::Term::Triple` in the reverse
    /// vector — `Term`'s `Hash + Eq` are recursive, so the existing
    /// `DashMap<Term, TermId>` deduplicates identical triple terms.
    TripleTerm = 6,
}

impl TermKind {
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(TermKind::Uri),
            1 => Some(TermKind::Blank),
            2 => Some(TermKind::PlainLiteral),
            3 => Some(TermKind::LangLiteral),
            4 => Some(TermKind::TypedLiteral),
            5 => Some(TermKind::InlineInt),
            6 => Some(TermKind::TripleTerm),
            _ => None,
        }
    }
}

pub(crate) const KIND_SHIFT: u32 = 60;
pub(crate) const PAYLOAD_MASK: u64 = (1u64 << KIND_SHIFT) - 1;
/// Maximum dictionary index that fits in the 60-bit payload (exclusive upper bound).
pub const MAX_DICT_INDEX: u64 = 1u64 << KIND_SHIFT;

impl TermId {
    pub fn new(kind: TermKind, payload: u64) -> Self {
        debug_assert!(payload < MAX_DICT_INDEX, "payload exceeds 60 bits");
        TermId(((kind as u64) << KIND_SHIFT) | payload)
    }

    pub fn kind(self) -> TermKind {
        TermKind::from_tag((self.0 >> KIND_SHIFT) as u8)
            .expect("term id has reserved/invalid kind tag")
    }

    pub fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }

    /// Raw 64-bit pattern (the SIMD batch decode reads these directly).
    #[inline]
    pub fn bits(self) -> u64 {
        self.0
    }

    pub fn inline_int(value: i32) -> Self {
        let payload = (value as u32) as u64; // zero-extend the 32-bit pattern
        TermId::new(TermKind::InlineInt, payload)
    }

    pub fn as_inline_int(self) -> Option<i32> {
        if self.kind() == TermKind::InlineInt {
            Some(self.payload() as u32 as i32)
        } else {
            None
        }
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct GraphId(pub u64);

/// Reserved sentinel for the default graph. Never collides with a `Uri`/`Blank`
/// dictionary index because the dictionary numbers terms starting from 1.
pub const DEFAULT_GRAPH: GraphId = GraphId(0);

/// A `(graph, subject, predicate, object)` quad of ids that came out of a
/// [`Dictionary`](crate::Dictionary).
///
/// The id-based store entry points ([`Store::insert_quad_ids`] and
/// [`Store::apply_quad_ids`](crate::Store::apply_quad_ids)) take this instead
/// of a bare `(GraphId, TermId, TermId, TermId)` tuple. Its field is private
/// and it implements no trait that can construct one, so
/// [`Dictionary::intern_quad`] is the only way to obtain one outside this
/// crate: a caller cannot reach storage with ids that were never interned.
/// The dictionary contract — intern exactly once per new term, in document
/// order — is therefore carried by the type, not by a comment.
///
/// It does **not** say *which* dictionary. See [`Store::apply_quad_ids`] for
/// the requirement the caller still owns.
///
/// `repr(transparent)` over the tuple the [`Tier`](crate::Tier) API already
/// speaks, so a batch converts to that shape with no copy — see
/// [`InternedQuad::peel_slice`].
///
/// [`Store::insert_quad_ids`]: crate::Store::insert_quad_ids
/// [`Store::apply_quad_ids`]: crate::Store::apply_quad_ids
/// [`Dictionary::intern_quad`]: crate::Dictionary::intern_quad
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct InternedQuad((GraphId, TermId, TermId, TermId));

impl InternedQuad {
    /// Pair a graph id with three term ids that already exist in a dictionary.
    /// Crate-internal on purpose: outside `horndb-storage` the only way in is
    /// [`Dictionary::intern_quad`](crate::Dictionary::intern_quad).
    pub(crate) fn from_ids(g: GraphId, s: TermId, p: TermId, o: TermId) -> Self {
        Self((g, s, p, o))
    }

    /// View a batch as the tuple slice [`Tier`](crate::Tier) takes, with no
    /// copy.
    ///
    /// Hand-written rather than derived from `bytemuck::TransparentWrapper`:
    /// that trait's `wrap`/`wrap_slice` are safe provided methods, so deriving
    /// it would hand every crate with `bytemuck` in scope a public constructor
    /// and undo the guard above.
    pub(crate) fn peel_slice(quads: &[Self]) -> &[(GraphId, TermId, TermId, TermId)] {
        // SAFETY: `InternedQuad` is `repr(transparent)` over exactly this
        // tuple, so the two have identical layout and validity.
        unsafe { std::slice::from_raw_parts(quads.as_ptr().cast(), quads.len()) }
    }

    pub fn graph(self) -> GraphId {
        self.0 .0
    }

    pub fn subject(self) -> TermId {
        self.0 .1
    }

    pub fn predicate(self) -> TermId {
        self.0 .2
    }

    pub fn object(self) -> TermId {
        self.0 .3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_uri() {
        let id = TermId::new(TermKind::Uri, 42);
        assert_eq!(id.kind(), TermKind::Uri);
        assert_eq!(id.payload(), 42);
    }

    #[test]
    fn pack_unpack_all_kinds() {
        for &k in &[
            TermKind::Uri,
            TermKind::Blank,
            TermKind::PlainLiteral,
            TermKind::LangLiteral,
            TermKind::TypedLiteral,
        ] {
            let id = TermId::new(k, 0xDEAD_BEEF);
            assert_eq!(id.kind(), k);
            assert_eq!(id.payload(), 0xDEAD_BEEF);
        }
    }

    #[test]
    fn inline_int_round_trip_positive() {
        let id = TermId::inline_int(123_456);
        assert_eq!(id.kind(), TermKind::InlineInt);
        assert_eq!(id.as_inline_int(), Some(123_456));
    }

    #[test]
    fn inline_int_round_trip_negative() {
        let id = TermId::inline_int(-1);
        assert_eq!(id.as_inline_int(), Some(-1));
        let id = TermId::inline_int(i32::MIN);
        assert_eq!(id.as_inline_int(), Some(i32::MIN));
    }

    #[test]
    fn non_int_returns_none_for_inline_int() {
        let id = TermId::new(TermKind::Uri, 7);
        assert_eq!(id.as_inline_int(), None);
    }

    #[test]
    fn default_graph_distinct_from_any_dictionary_id() {
        assert_eq!(DEFAULT_GRAPH.0, 0);
        assert_ne!(DEFAULT_GRAPH.0, TermId::new(TermKind::Uri, 1).0);
    }

    #[test]
    fn triple_term_kind_round_trips() {
        let id = TermId::new(TermKind::TripleTerm, 42);
        assert_eq!(id.kind(), TermKind::TripleTerm);
        assert_eq!(id.payload(), 42);
        // Tag round-trip via the public `from_tag` path.
        assert_eq!(TermKind::from_tag(6), Some(TermKind::TripleTerm));
        // Distinct from every other kind for the same payload.
        for &k in &[
            TermKind::Uri,
            TermKind::Blank,
            TermKind::PlainLiteral,
            TermKind::LangLiteral,
            TermKind::TypedLiteral,
            TermKind::InlineInt,
        ] {
            assert_ne!(id.0, TermId::new(k, 42).0);
        }
    }
}
