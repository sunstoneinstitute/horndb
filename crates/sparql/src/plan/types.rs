//! Binding/type lattice (SPEC-23 §5.1, ported from Oxigraph `sparopt`'s
//! `type_inference.rs`). A [`TypeMask`] is a 5-bit set over
//! `{UNDEF, NAMED_NODE, BLANK_NODE, LITERAL, TRIPLE}`; a variable is *bound*
//! iff its `UNDEF` bit is clear. [`infer`] propagates masks bottom-up over a
//! [`LogicalPlan`]. This single sound-by-construction pass is what
//! filter-pushdown legality, join-key discovery, and the WCOJ-vs-hash
//! decision (Phase 2+) will read.

use crate::algebra::{Term, TriplePattern, Var};
use crate::plan::logical::LogicalPlan;
use std::collections::HashMap;

/// A set of possible term kinds for one variable. Bit layout matches
/// `sparopt`'s `VariableType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeMask(u8);

impl TypeMask {
    /// Variable may be *unbound* in some solution.
    pub const UNDEF: u8 = 0b0_0001;
    /// May be an IRI.
    pub const NAMED_NODE: u8 = 0b0_0010;
    /// May be a blank node.
    pub const BLANK_NODE: u8 = 0b0_0100;
    /// May be a literal.
    pub const LITERAL: u8 = 0b0_1000;
    /// May be an RDF 1.2 triple term.
    pub const TRIPLE: u8 = 0b1_0000;
    /// Any bound RDF term (no `UNDEF`).
    pub const ANY: u8 = Self::NAMED_NODE | Self::BLANK_NODE | Self::LITERAL | Self::TRIPLE;

    /// Construct from a raw bit set.
    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
    /// The raw bit set.
    pub fn bits(&self) -> u8 {
        self.0
    }
    /// A definitely-bound variable is one whose `UNDEF` bit is clear.
    pub fn is_bound(&self) -> bool {
        self.0 & Self::UNDEF == 0
    }
    /// Union two masks (set of possibilities on either branch).
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    /// Join semantics for a shared variable: bound iff bound on either side,
    /// and the type set is the intersection **plus each side's types when the
    /// other side may be unbound** — SPARQL join compatibility lets an unbound
    /// var adopt the other side's value, so those cross-terms must survive
    /// (dropping them would let a Phase-2 pass fold e.g. `isLiteral(?x)` to a
    /// wrong constant).
    pub fn intersect(self, other: Self) -> Self {
        let mut types = self.0 & other.0 & Self::ANY;
        if other.0 & Self::UNDEF != 0 {
            types |= self.0 & Self::ANY;
        }
        if self.0 & Self::UNDEF != 0 {
            types |= other.0 & Self::ANY;
        }
        let undef = (self.0 & other.0) & Self::UNDEF;
        Self(types | undef)
    }
    /// Return this mask with the `UNDEF` bit set (may be absent).
    pub fn with_undef(self) -> Self {
        Self(self.0 | Self::UNDEF)
    }
}

/// Inferred [`TypeMask`] per variable produced by a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarTypes(HashMap<Var, TypeMask>);

impl VarTypes {
    /// Mask for `var`, if the plan produces it.
    pub fn get(&self, var: &Var) -> Option<TypeMask> {
        self.0.get(var).copied()
    }
    /// The set of variables the plan may bind.
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.0.keys()
    }
    fn insert_union(&mut self, var: Var, mask: TypeMask) {
        self.0
            .entry(var)
            .and_modify(|m| *m = m.union(mask))
            .or_insert(mask);
    }
}

/// The bound mask a variable receives when it appears in a given triple-pattern
/// position (subject / predicate / object).
fn subject_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::NAMED_NODE | TypeMask::BLANK_NODE)
}
fn predicate_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::NAMED_NODE)
}
fn object_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::ANY)
}

fn add_pattern_vars(p: &TriplePattern, out: &mut VarTypes) {
    add_term_vars(&p.subject, subject_mask(), out);
    add_term_vars(&p.predicate, predicate_mask(), out);
    add_term_vars(&p.object, object_mask(), out);
}

fn add_term_vars(t: &Term, mask: TypeMask, out: &mut VarTypes) {
    match t {
        Term::Var(v) => out.insert_union(v.clone(), mask),
        // RDF 1.2 triple term: its inner variables are bound by the outer
        // pattern's position; recurse with the same mask (sound over-approx).
        Term::Triple(tp) => add_pattern_vars(tp, out),
        _ => {}
    }
}

/// Bottom-up type inference over a [`LogicalPlan`].
pub fn infer(plan: &LogicalPlan) -> VarTypes {
    use LogicalPlan::*;
    match plan {
        Bgp { patterns } => {
            let mut vt = VarTypes::default();
            for p in patterns {
                add_pattern_vars(p, &mut vt);
            }
            vt
        }
        // Join: shared vars intersect; each side's own vars pass through.
        Join { left, right } => {
            let r = infer(right);
            let mut out = infer(left);
            for (v, rm) in r.0 {
                match out.0.get(&v).copied() {
                    Some(lm) => {
                        out.0.insert(v, lm.intersect(rm));
                    }
                    None => {
                        out.0.insert(v, rm);
                    }
                }
            }
            out
        }
        // LeftJoin: left as-is; right-only vars become optional (UNDEF).
        LeftJoin { left, right, .. } => {
            let r = infer(right);
            let mut out = infer(left);
            for (v, rm) in r.0 {
                match out.0.get(&v).copied() {
                    Some(lm) => {
                        out.0.insert(v, lm.union(rm));
                    }
                    None => {
                        out.0.insert(v, rm.with_undef());
                    }
                }
            }
            out
        }
        // Union: shared vars union; one-sided vars union-with-UNDEF.
        Union { left, right } => {
            let l = infer(left);
            let r = infer(right);
            let mut out = VarTypes::default();
            for (v, m) in l.0 {
                let mask = if r.0.contains_key(&v) {
                    m
                } else {
                    m.with_undef()
                };
                out.0.insert(v, mask);
            }
            for (v, m) in r.0 {
                match out.0.get(&v).copied() {
                    Some(existing) => {
                        out.0.insert(v, existing.union(m));
                    }
                    None => {
                        out.0.insert(v, m.with_undef());
                    }
                }
            }
            out
        }
        Filter { inner, .. } | Slice { inner, .. } | OrderBy { inner, .. } | Distinct { inner } => {
            infer(inner)
        }
        Project { vars, inner } => {
            let child = infer(inner);
            let mut out = VarTypes::default();
            for v in vars {
                let mask = child
                    .get(v)
                    .unwrap_or_else(|| TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY));
                out.0.insert(v.clone(), mask);
            }
            out
        }
        // BIND: the bound expression may error → the new var is optional.
        Extend { inner, var, .. } => {
            let mut out = infer(inner);
            out.insert_union(
                var.clone(),
                TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY),
            );
            out
        }
        Values { vars, rows } => {
            let mut out = VarTypes::default();
            for (col, v) in vars.iter().enumerate() {
                let mut mask = 0u8;
                for row in rows {
                    match row.get(col).and_then(|c| c.as_ref()) {
                        None => mask |= TypeMask::UNDEF,
                        Some(t) => mask |= term_kind_bit(t),
                    }
                }
                out.0.insert(v.clone(), TypeMask::from_bits(mask));
            }
            out
        }
        Group {
            inner,
            keys,
            aggregates,
        } => {
            let child = infer(inner);
            let mut out = VarTypes::default();
            for k in keys {
                let mask = child
                    .get(k)
                    .unwrap_or_else(|| TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY));
                out.0.insert(k.clone(), mask);
            }
            // Aggregate output kind depends on the function. COUNT always
            // yields a bound integer literal. SUM/AVG/GROUP_CONCAT yield a
            // literal but may error (non-numeric input) → optional literal.
            // MIN/MAX/SAMPLE return one of the *input* terms (any kind) and
            // are unbound over an empty/erroring group → optional any.
            for a in aggregates {
                use crate::algebra::AggFunc::*;
                let mask = match &a.func {
                    CountStar | Count(_) => TypeMask::from_bits(TypeMask::LITERAL),
                    Sum(_) | Avg(_) | GroupConcat { .. } => {
                        TypeMask::from_bits(TypeMask::UNDEF | TypeMask::LITERAL)
                    }
                    Min(_) | Max(_) | Sample(_) => {
                        TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY)
                    }
                };
                out.0.insert(a.out.clone(), mask);
            }
            out
        }
        // Endpoint values come from whatever terms appear in the edge
        // relation — with inverse steps (`^p+`) even the source column can
        // hold literals, so both endpoints get the full bound mask (sound
        // over-approximation; tightening needs edge-shape analysis).
        PathClosure {
            subject, object, ..
        } => {
            let mut out = VarTypes::default();
            for t in [subject, object] {
                if let Term::Var(v) = t {
                    out.insert_union(v.clone(), TypeMask::from_bits(TypeMask::ANY));
                }
            }
            out
        }
    }
}

fn term_kind_bit(t: &Term) -> u8 {
    match t {
        Term::Iri(_) => TypeMask::NAMED_NODE,
        Term::BlankNode(_) => TypeMask::BLANK_NODE,
        Term::Literal(_) => TypeMask::LITERAL,
        Term::Triple(_) => TypeMask::TRIPLE,
        // A Var in a VALUES cell is not a ground term; treat as any-bound.
        Term::Var(_) => TypeMask::ANY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Expr;

    fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
        TriplePattern {
            subject: Term::Var(Var::new(s)),
            predicate: Term::Iri(p.to_owned()),
            object: Term::Var(Var::new(o)),
        }
    }
    fn bgp(pats: Vec<TriplePattern>) -> LogicalPlan {
        LogicalPlan::Bgp { patterns: pats }
    }

    #[test]
    fn bgp_binds_positions() {
        let vt = infer(&bgp(vec![pat("s", "p", "o")]));
        let s = vt.get(&Var::new("s")).unwrap();
        assert!(s.is_bound());
        // subject can be IRI or blank, never literal
        assert_eq!(s.bits() & TypeMask::LITERAL, 0);
        let o = vt.get(&Var::new("o")).unwrap();
        assert!(o.is_bound());
        assert_ne!(o.bits() & TypeMask::LITERAL, 0);
    }

    #[test]
    fn leftjoin_marks_rhs_only_vars_undef() {
        // { ?s :p ?o OPTIONAL { ?s :q ?x } } — ?x may be unbound.
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("s", "q", "x")]);
        let vt = infer(&LogicalPlan::LeftJoin {
            left: Box::new(left),
            right: Box::new(right),
            expr: None,
        });
        assert!(vt.get(&Var::new("s")).unwrap().is_bound(), "?s stays bound");
        assert!(
            !vt.get(&Var::new("x")).unwrap().is_bound(),
            "?x is optional → UNDEF set"
        );
    }

    #[test]
    fn union_marks_one_sided_vars_undef() {
        // { { ?x :p ?a } UNION { ?x :q ?b } } — ?a and ?b each one-sided.
        let left = bgp(vec![pat("x", "p", "a")]);
        let right = bgp(vec![pat("x", "q", "b")]);
        let vt = infer(&LogicalPlan::Union {
            left: Box::new(left),
            right: Box::new(right),
        });
        assert!(
            vt.get(&Var::new("x")).unwrap().is_bound(),
            "?x on both sides"
        );
        assert!(!vt.get(&Var::new("a")).unwrap().is_bound());
        assert!(!vt.get(&Var::new("b")).unwrap().is_bound());
    }

    #[test]
    fn join_keeps_shared_var_bound() {
        // Join of two BGPs sharing ?o (built raw, not coalesced).
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("o", "q", "z")]);
        let vt = infer(&LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(right),
        });
        assert!(vt.get(&Var::new("o")).unwrap().is_bound());
        // ?o is a subject on the right, an object on the left → intersection
        // excludes LITERAL (subject can't be a literal).
        assert_eq!(
            vt.get(&Var::new("o")).unwrap().bits() & TypeMask::LITERAL,
            0
        );
    }

    #[test]
    fn join_with_optional_side_keeps_cross_type_terms() {
        // If ?x may be UNDEF on the left (e.g. it came from an OPTIONAL),
        // a join row can adopt the right side's value — so the right side's
        // type bits must survive the intersection.
        let left =
            TypeMask::from_bits(TypeMask::UNDEF | TypeMask::NAMED_NODE | TypeMask::BLANK_NODE);
        let right = TypeMask::from_bits(TypeMask::LITERAL);
        let joined = left.intersect(right);
        assert!(joined.is_bound(), "bound on one side → bound after join");
        assert_ne!(
            joined.bits() & TypeMask::LITERAL,
            0,
            "right side's LITERAL must survive: left may be unbound"
        );
        // Both definitely bound → plain intersection (no cross-terms).
        let strict = TypeMask::from_bits(TypeMask::NAMED_NODE | TypeMask::LITERAL)
            .intersect(TypeMask::from_bits(TypeMask::NAMED_NODE));
        assert_eq!(strict.bits(), TypeMask::NAMED_NODE);
    }

    #[test]
    fn aggregate_output_masks_depend_on_function() {
        use crate::algebra::{AggFunc, Aggregate};
        let inner = bgp(vec![pat("s", "p", "o")]);
        let agg = |func: AggFunc, name: &str| Aggregate {
            out: Var::new(name),
            func,
            distinct: false,
        };
        let vt = infer(&LogicalPlan::Group {
            inner: Box::new(inner),
            keys: vec![],
            aggregates: vec![
                agg(AggFunc::CountStar, "c"),
                agg(
                    AggFunc::Sum(Box::new(Expr::Term(Term::Var(Var::new("o"))))),
                    "sum",
                ),
                agg(
                    AggFunc::Sample(Box::new(Expr::Term(Term::Var(Var::new("s"))))),
                    "sample",
                ),
            ],
        });
        // COUNT: always a bound literal.
        assert!(vt.get(&Var::new("c")).unwrap().is_bound());
        // SUM: literal but may error → optional.
        assert!(!vt.get(&Var::new("sum")).unwrap().is_bound());
        // SAMPLE: returns an input term — may be an IRI, may be unbound.
        let sample = vt.get(&Var::new("sample")).unwrap();
        assert!(!sample.is_bound());
        assert_ne!(sample.bits() & TypeMask::NAMED_NODE, 0);
    }

    #[test]
    fn path_closure_endpoints_may_bind_literals() {
        // `:a <p>+ ?y` with data `:a :p "lit"` binds ?y to a literal — the
        // endpoint mask must not exclude LITERAL.
        let vt = infer(&LogicalPlan::PathClosure {
            subject: Term::Iri("http://ex/a".into()),
            object: Term::Var(Var::new("y")),
            edge: Box::new(bgp(vec![pat("pp_src", "http://ex/p", "pp_dst")])),
            reflexive: false,
        });
        let y = vt.get(&Var::new("y")).unwrap();
        assert!(y.is_bound());
        assert_ne!(y.bits() & TypeMask::LITERAL, 0);
    }

    #[test]
    fn extend_output_is_optional() {
        let inner = bgp(vec![pat("s", "p", "o")]);
        let vt = infer(&LogicalPlan::Extend {
            inner: Box::new(inner),
            var: Var::new("b"),
            expr: Expr::Bound(Var::new("o")),
        });
        assert!(!vt.get(&Var::new("b")).unwrap().is_bound());
    }
}
