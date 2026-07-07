//! Value-space intersection narrowing of `rdfs:range` declarations.
//!
//! This is a **third** small datatype pass, alongside [`crate::datatypes`]
//! (datatype-IRI subsumption) and [`crate::datatype_literals`] (instance
//! literal value reasoning). It covers a gap neither of those can close: a
//! property declared with **two or more** `rdfs:range` datatypes whose value
//! spaces *intersect* into something narrower than any single declared
//! range.
//!
//! The existing `scm-rng1` rule only walks the `rdfs:subClassOf` lattice
//! **upward** — from a declared range to its declared supertypes (e.g.
//! `xsd:short` ⟶ `xsd:int` ⟶ `xsd:long` ⟶ `xsd:integer` ⟶ `xsd:decimal`). It
//! has no way to derive a range that is *narrower* than anything declared,
//! because no single `subClassOf` edge encodes an intersection of two
//! branches of the lattice. Two W3C conformance cases need exactly that:
//!
//! - `WebOnt-I5.8-008-pe`: `p rdfs:range xsd:short` + `p rdfs:range
//!   xsd:unsignedInt` entails `p rdfs:range xsd:unsignedShort` (short ∩
//!   unsignedInt = `[0, 32767]` ⊆ unsignedShort's `[0, 65535]`).
//! - `WebOnt-I5.8-009-pe`: `p rdfs:range xsd:nonNegativeInteger` + `p
//!   rdfs:range xsd:nonPositiveInteger` entails `p rdfs:range xsd:short`
//!   (`[0, ∞) ∩ (−∞, 0] = {0}` ⊆ short's `[−32768, 32767]`).
//!
//! ## Value-space model
//!
//! Every XSD numeric-tower datatype's value space is modelled as an integer
//! interval `[lo, hi]` where each bound is `Option<i128>` — `None` means
//! unbounded (−∞ for `lo`, +∞ for `hi`). `xsd:decimal` is a distinguished
//! superset of every integer value space (the reals) and contributes no
//! bound to an intersection. Every other datatype (`xsd:string`,
//! `xsd:boolean`, `xsd:dateTime`, `xsd:dateTimeStamp`, any unknown/user IRI)
//! is **opaque**: a property that declares an opaque range anywhere in its
//! range set is left untouched by this pass — value-space reasoning never
//! crosses into an opaque datatype.
//!
//! ## Soundness: supersets only
//!
//! For a property `p` with declared ranges `D_p`, every actual value of `p`
//! lies in the intersection of `D_p`'s value spaces (that is what
//! `rdfs:range` conjunction means under OWL 2 RL). This pass only ever
//! *adds* a range `T` when `T`'s value space is a **superset** of that
//! intersection. Consequently no value that was ever legal for `p` becomes
//! illegal by the addition of `T` — the pass can never manufacture a false
//! `dt-not-type` inconsistency. It also never asserts a range narrower than
//! the true intersection, so it never rules out a value that was legal
//! before.
//!
//! ## Why ≥2 declared ranges
//!
//! A property with a single declared range is left entirely to `scm-rng1`
//! (which already broadens it correctly along the lattice); comparing a
//! lone datatype against itself is a no-op intersection and would only add
//! noise. Requiring at least two distinct declared range datatypes also
//! keeps this pass from ever firing on ordinary single-range ontologies.

use crate::store::{MemStore, TripleStore};
use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;
use rustc_hash::{FxHashMap, FxHashSet};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// A value-space interval `[lo, hi]`. `None` is unbounded: −∞ for `lo`, +∞
/// for `hi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interval {
    lo: Option<i128>,
    hi: Option<i128>,
}

impl Interval {
    const fn new(lo: Option<i128>, hi: Option<i128>) -> Self {
        Self { lo, hi }
    }
}

/// `(local name, value-space interval)` for every XSD numeric-tower
/// datatype this pass reasons over, including the unbounded `integer`.
/// `xsd:decimal` is deliberately excluded — it is handled separately as a
/// no-bound superset (see the module doc).
const NUMERIC_INTERVALS: &[(&str, Interval)] = &[
    ("byte", Interval::new(Some(-128), Some(127))),
    ("short", Interval::new(Some(-32768), Some(32767))),
    ("int", Interval::new(Some(-2147483648), Some(2147483647))),
    (
        "long",
        Interval::new(Some(-9223372036854775808), Some(9223372036854775807)),
    ),
    ("unsignedByte", Interval::new(Some(0), Some(255))),
    ("unsignedShort", Interval::new(Some(0), Some(65535))),
    ("unsignedInt", Interval::new(Some(0), Some(4294967295))),
    (
        "unsignedLong",
        Interval::new(Some(0), Some(18446744073709551615)),
    ),
    ("nonNegativeInteger", Interval::new(Some(0), None)),
    ("positiveInteger", Interval::new(Some(1), None)),
    ("nonPositiveInteger", Interval::new(None, Some(0))),
    ("negativeInteger", Interval::new(None, Some(-1))),
    ("integer", Interval::new(None, None)),
];

/// The local name of `xsd:decimal` — the universal integer-superset,
/// modelled outside [`NUMERIC_INTERVALS`] because it is not itself an
/// interval (it is the reals) and contributes no bound to an intersection.
const DECIMAL: &str = "decimal";

/// `a <= b` for two "lo"-type bounds (`None` = −∞).
fn lo_le(a: Option<i128>, b: Option<i128>) -> bool {
    match (a, b) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a <= b,
    }
}

/// `a >= b` for two "hi"-type bounds (`None` = +∞).
fn hi_ge(a: Option<i128>, b: Option<i128>) -> bool {
    match (a, b) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a >= b,
    }
}

/// The greater of two "lo"-type bounds (`None` = −∞) — narrows an
/// intersection's lower bound.
fn lo_max(a: Option<i128>, b: Option<i128>) -> Option<i128> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.max(b)),
    }
}

/// The lesser of two "hi"-type bounds (`None` = +∞) — narrows an
/// intersection's upper bound.
fn hi_min(a: Option<i128>, b: Option<i128>) -> Option<i128> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.min(b)),
    }
}

/// True iff the interval `t` (a candidate target datatype's value space) is
/// a superset of `i` (the computed intersection) — i.e. `t` is eligible to
/// be derived as a range.
fn is_superset(t: Interval, i: Interval) -> bool {
    lo_le(t.lo, i.lo) && hi_ge(t.hi, i.hi)
}

/// Intersect the value-space intervals of `members` (local XSD names).
///
/// Returns `None` if every member is [`DECIMAL`] (no integer-tower member to
/// bound the intersection — nothing to narrow). `members` must not be
/// empty and must contain only names present in [`NUMERIC_INTERVALS`] or
/// equal to [`DECIMAL`] (opaque names are filtered out by the caller before
/// this is called).
fn intersect(members: &[&str]) -> Option<Interval> {
    let mut lo: Option<i128> = None;
    let mut hi: Option<i128> = None;
    let mut has_integer_member = false;
    for &name in members {
        if name == DECIMAL {
            continue;
        }
        let Some(&(_, iv)) = NUMERIC_INTERVALS.iter().find(|(n, _)| *n == name) else {
            debug_assert!(false, "unknown numeric-tower name {name}");
            continue;
        };
        has_integer_member = true;
        lo = lo_max(lo, iv.lo);
        hi = hi_min(hi, iv.hi);
    }
    if !has_integer_member {
        return None;
    }
    Some(Interval::new(lo, hi))
}

/// Derive narrowed `rdfs:range` declarations from value-space intersection.
///
/// For every property `p` with **two or more** distinct asserted `p
/// rdfs:range <D>` datatypes, all of which are known numeric-tower
/// datatypes or `xsd:decimal` (a single opaque/unknown datatype among `p`'s
/// declared ranges disqualifies `p` entirely — see the module doc), compute
/// the intersection of their value spaces and assert `p rdfs:range <T>` for
/// every candidate `T` (every [`NUMERIC_INTERVALS`] entry plus `xsd:decimal`)
/// whose value space is a superset of that intersection. Triples already
/// present are left untouched (checked via `store.contains`).
///
/// `intern` resolves an IRI to its `TermId`, exactly like
/// [`crate::datatypes::inject_datatype_axioms`]. This pass pre-interns
/// every known numeric-tower datatype IRI (plus `xsd:decimal`) up front to
/// build a `TermId -> local name` lookup — the simplest way to recover the
/// IRI behind a `rdfs:range` object `TermId` without needing read access to
/// the caller's dictionary.
///
/// Must run **after** the range data is loaded and **before**
/// materialization, so `scm-rng1` / `prp-rng` propagate the derived ranges
/// during the fixpoint.
pub fn derive_range_intersections(
    store: &mut MemStore,
    vocab: &Vocabulary,
    mut intern: impl FnMut(&str) -> TermId,
) {
    // TermId -> local XSD name, for every datatype this pass understands.
    let mut known: FxHashMap<TermId, &'static str> = FxHashMap::default();
    for &(name, _) in NUMERIC_INTERVALS {
        known.insert(intern(&format!("{XSD}{name}")), name);
    }
    known.insert(intern(&format!("{XSD}{DECIMAL}")), DECIMAL);

    // Group asserted `?p rdfs:range ?d` by subject property, deduping range
    // objects (repeats of the same declared range must not count twice
    // toward the "≥2 distinct" gate).
    let mut by_property: FxHashMap<TermId, FxHashSet<TermId>> = FxHashMap::default();
    for t in store.scan_predicate(vocab.rdfs_range) {
        by_property.entry(t.s).or_default().insert(t.o);
    }

    for (prop, ranges) in by_property {
        if ranges.len() < 2 {
            continue;
        }

        // Any range datatype this pass doesn't recognise disqualifies the
        // property entirely — never cross-compare against an opaque space.
        let mut names: Vec<&str> = Vec::with_capacity(ranges.len());
        let mut opaque = false;
        for r in &ranges {
            match known.get(r) {
                Some(&name) => names.push(name),
                None => {
                    opaque = true;
                    break;
                }
            }
        }
        if opaque {
            continue;
        }

        let Some(intersection) = intersect(&names) else {
            // All-decimal: nothing to narrow.
            continue;
        };
        if let (Some(lo), Some(hi)) = (intersection.lo, intersection.hi) {
            if lo > hi {
                // Empty/contradictory intersection — degenerate, derive
                // nothing rather than assert a bogus range.
                continue;
            }
        }

        for &(target, iv) in NUMERIC_INTERVALS {
            if is_superset(iv, intersection) {
                assert_range(store, vocab, &mut intern, prop, target);
            }
        }
        // xsd:decimal is always a superset of a bounded integer
        // intersection (we only reach here when `has_integer_member` was
        // true, i.e. the intersection is genuinely integer-shaped).
        assert_range(store, vocab, &mut intern, prop, DECIMAL);
    }
}

/// Assert `prop rdfs:range <XSD local>` as a base fact iff not already
/// present.
fn assert_range(
    store: &mut MemStore,
    vocab: &Vocabulary,
    intern: &mut impl FnMut(&str) -> TermId,
    prop: TermId,
    local: &str,
) {
    let target = intern(&format!("{XSD}{local}"));
    let triple = Triple::new(prop, vocab.rdfs_range, target);
    if !store.contains(&triple) {
        store.assert(triple);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap as TestMap;

    // ---- interval helpers -------------------------------------------------

    #[test]
    fn lo_le_handles_infinities() {
        assert!(lo_le(None, None)); // -inf <= -inf
        assert!(lo_le(None, Some(0))); // -inf <= anything
        assert!(!lo_le(Some(0), None)); // finite > -inf
        assert!(lo_le(Some(0), Some(0)));
        assert!(lo_le(Some(-5), Some(0)));
        assert!(!lo_le(Some(5), Some(0)));
    }

    #[test]
    fn hi_ge_handles_infinities() {
        assert!(hi_ge(None, None)); // +inf >= +inf
        assert!(hi_ge(None, Some(0))); // +inf >= anything
        assert!(!hi_ge(Some(0), None)); // finite < +inf
        assert!(hi_ge(Some(0), Some(0)));
        assert!(hi_ge(Some(5), Some(0)));
        assert!(!hi_ge(Some(-5), Some(0)));
    }

    #[test]
    fn lo_max_and_hi_min_are_infinity_identities() {
        assert_eq!(lo_max(None, Some(5)), Some(5));
        assert_eq!(lo_max(Some(5), None), Some(5));
        assert_eq!(lo_max(None, None), None);
        assert_eq!(lo_max(Some(5), Some(9)), Some(9));

        assert_eq!(hi_min(None, Some(5)), Some(5));
        assert_eq!(hi_min(Some(5), None), Some(5));
        assert_eq!(hi_min(None, None), None);
        assert_eq!(hi_min(Some(5), Some(9)), Some(5));
    }

    fn interval_of(local: &str) -> Interval {
        NUMERIC_INTERVALS
            .iter()
            .find(|(n, _)| *n == local)
            .unwrap()
            .1
    }

    /// short ∩ unsignedInt = `[0, 32767]`: `unsignedShort` is eligible,
    /// `positiveInteger` and `byte` are not (WebOnt-I5.8-008-pe).
    #[test]
    fn intersection_short_and_unsigned_int() {
        let i = intersect(&["short", "unsignedInt"]).unwrap();
        assert_eq!(i, Interval::new(Some(0), Some(32767)));

        assert!(is_superset(interval_of("unsignedShort"), i));
        assert!(!is_superset(interval_of("positiveInteger"), i));
        assert!(!is_superset(interval_of("byte"), i));
    }

    /// nonNegativeInteger ∩ nonPositiveInteger = `[0, 0]`: `short` is
    /// eligible, `positiveInteger` and `negativeInteger` are not
    /// (WebOnt-I5.8-009-pe).
    #[test]
    fn intersection_nonneg_and_nonpos() {
        let i = intersect(&["nonNegativeInteger", "nonPositiveInteger"]).unwrap();
        assert_eq!(i, Interval::new(Some(0), Some(0)));

        assert!(is_superset(interval_of("short"), i));
        assert!(!is_superset(interval_of("positiveInteger"), i));
        assert!(!is_superset(interval_of("negativeInteger"), i));
    }

    #[test]
    fn intersection_all_decimal_has_nothing_to_narrow() {
        assert_eq!(intersect(&[DECIMAL, DECIMAL]), None);
    }

    #[test]
    fn intersection_with_decimal_member_ignores_its_bound() {
        // decimal contributes no bound; intersecting short with decimal is
        // just short's own interval.
        let i = intersect(&["short", DECIMAL]).unwrap();
        assert_eq!(i, interval_of("short"));
    }

    #[test]
    fn empty_intersection_is_detected() {
        // positiveInteger [1, +inf) ∩ negativeInteger (-inf, -1] is empty.
        let i = intersect(&["positiveInteger", "negativeInteger"]).unwrap();
        assert!(matches!((i.lo, i.hi), (Some(lo), Some(hi)) if lo > hi));
    }

    // ---- store-level pass ---------------------------------------------------

    /// A closure handing out incrementing synthetic `TermId`s, deduping by
    /// IRI exactly like a real dictionary would (mirrors the helper in
    /// `datatypes.rs`'s tests).
    fn synthetic_interner(base: u64) -> impl FnMut(&str) -> TermId {
        let mut map: TestMap<String, TermId> = TestMap::default();
        let mut next = base;
        move |iri: &str| -> TermId {
            if let Some(&t) = map.get(iri) {
                return t;
            }
            let t = TermId(next);
            next += 1;
            map.insert(iri.to_string(), t);
            t
        }
    }

    fn xsd(local: &str) -> String {
        format!("{XSD}{local}")
    }

    #[test]
    fn store_level_short_and_unsigned_int_derives_unsigned_short() {
        let vocab = Vocabulary::synthetic(1);
        let mut store = MemStore::new(vocab);

        let mut intern = synthetic_interner(1000);
        let p = intern("urn:example:p");
        let short = intern(&xsd("short"));
        let unsigned_int = intern(&xsd("unsignedInt"));

        store.assert(Triple::new(p, vocab.rdfs_range, short));
        store.assert(Triple::new(p, vocab.rdfs_range, unsigned_int));

        derive_range_intersections(&mut store, &vocab, &mut intern);

        let unsigned_short = intern(&xsd("unsignedShort"));
        assert!(store.contains(&Triple::new(p, vocab.rdfs_range, unsigned_short)));

        // Must not derive an ineligible narrower/incomparable type.
        let byte = intern(&xsd("byte"));
        assert!(!store.contains(&Triple::new(p, vocab.rdfs_range, byte)));
        let positive_integer = intern(&xsd("positiveInteger"));
        assert!(!store.contains(&Triple::new(p, vocab.rdfs_range, positive_integer)));
    }

    #[test]
    fn store_level_single_range_derives_nothing() {
        let vocab = Vocabulary::synthetic(1);
        let mut store = MemStore::new(vocab);

        let mut intern = synthetic_interner(1000);
        let p = intern("urn:example:p");
        let short = intern(&xsd("short"));
        store.assert(Triple::new(p, vocab.rdfs_range, short));

        let before = store.all_triples().len();
        derive_range_intersections(&mut store, &vocab, &mut intern);
        let after = store.all_triples().len();

        // Only the pre-interning of known datatypes' assertions could have
        // changed the store, and this pass never asserts anything for
        // properties with <2 distinct declared ranges.
        assert_eq!(before, after, "single-range property must be untouched");
    }

    #[test]
    fn store_level_opaque_range_derives_nothing() {
        let vocab = Vocabulary::synthetic(1);
        let mut store = MemStore::new(vocab);

        let mut intern = synthetic_interner(1000);
        let p = intern("urn:example:p");
        let string_dt = intern(&xsd("string"));
        let int_dt = intern(&xsd("int"));
        store.assert(Triple::new(p, vocab.rdfs_range, string_dt));
        store.assert(Triple::new(p, vocab.rdfs_range, int_dt));

        let before = store.all_triples().len();
        derive_range_intersections(&mut store, &vocab, &mut intern);
        let after = store.all_triples().len();
        assert_eq!(
            before, after,
            "a property with an opaque declared range must be untouched"
        );
    }
}
