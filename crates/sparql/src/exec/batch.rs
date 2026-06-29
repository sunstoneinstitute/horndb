//! Slot rows — the id-carrying runtime row that replaces the
//! string-decoded `Bindings` above the executor seam (#128).
//!
//! Within-column homogeneity invariant: in a single `Batch`, a given slot
//! index is uniformly `Id`, uniformly `Term`, or uniformly `Unbound` across
//! rows (scan ⇒ all `Id`; `from_bindings` ⇒ all `Term`; native operators
//! preserve per-column provenance). Equality keys may therefore hash on raw
//! ids without an `Id`-vs-equal-`Term` collision; the `Slot::eq` decode path
//! is the correctness backstop for genuinely mixed comparisons.

use crate::algebra::{Term, Var};
use crate::error::Result;
use crate::exec::Bindings;
use horndb_storage::TermId;

/// One cell of a solution row.
// Derived `PartialEq` is STRUCTURAL (needed for `Row`/`Batch` equality in tests).
// It is NOT SPARQL term-identity — use `Slot::eq` for term identity across a decode boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// Dictionary id straight from the scan — the hot, no-string case.
    Id(TermId),
    /// A materialized term: BIND/aggregate output, VALUES literal, or a
    /// path-synthesized endpoint.
    Term(Term),
    /// No binding (OPTIONAL right-side with no match).
    Unbound,
}

impl Slot {
    /// SPARQL term-identity equality. `Id == Id` compares ids directly (no
    /// decode); any other mix decodes both sides and compares by term.
    /// `Unbound` equals only `Unbound`.
    pub fn eq(a: &Slot, b: &Slot, decode: impl Fn(TermId) -> Result<Term>) -> Result<bool> {
        Ok(match (a, b) {
            (Slot::Id(x), Slot::Id(y)) => x == y,
            (Slot::Unbound, Slot::Unbound) => true,
            (Slot::Unbound, _) | (_, Slot::Unbound) => false,
            _ => {
                let ta = a.to_term(&decode)?;
                let tb = b.to_term(&decode)?;
                ta == tb
            }
        })
    }

    /// Decode this slot to a `Term`. `Unbound` is an error (callers must
    /// check boundness first); `Id` goes through `decode`, `Term` clones.
    fn to_term(&self, decode: &impl Fn(TermId) -> Result<Term>) -> Result<Term> {
        match self {
            Slot::Id(id) => decode(*id),
            Slot::Term(t) => Ok(t.clone()),
            Slot::Unbound => Err(crate::error::SparqlError::Executor(
                "to_term on Unbound slot".into(),
            )),
        }
    }

    /// A hash/Ord key for equality grouping (Distinct / Group / Join key).
    /// Relies on within-column homogeneity (see module docs): `Id` keys on
    /// the raw id, `Term` on its lexical form, so two homogeneous columns
    /// never produce a false `Id`-vs-`Term` collision.
    pub fn key_part(&self) -> KeyPart {
        match self {
            Slot::Id(id) => KeyPart::Id(id.0),
            Slot::Term(t) => KeyPart::Lex(crate::exec::runtime::lex(t)),
            Slot::Unbound => KeyPart::Unbound,
        }
    }
}

/// A grouping/equality key fragment for one slot. `Ord` so group output can
/// be made deterministic; `Lex` carries the term's lexical form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyPart {
    Id(u64),
    Lex(String),
    Unbound,
}

/// One solution row: `slots[i]` is the value of `schema[i]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row(pub Vec<Slot>);

/// A block of rows sharing one schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub schema: Vec<Var>,
    pub rows: Vec<Row>,
}

impl Batch {
    /// Zero rows over the empty schema.
    pub fn empty() -> Self {
        Batch {
            schema: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// One empty row over the empty schema — the BGP/ASK unit ("one empty
    /// solution mapping").
    pub fn unit() -> Self {
        Batch {
            schema: Vec::new(),
            rows: vec![Row(Vec::new())],
        }
    }

    /// Slot index of `var`, if present in the schema.
    pub fn col(&self, var: &str) -> Option<usize> {
        self.schema.iter().position(|v| v.name() == var)
    }

    /// Wrap decoded `Bindings` rows as a `Batch` of `Slot::Term` cells (the
    /// inverse of `to_bindings`). The schema is the sorted union of all bound
    /// variable names; a row that does not bind a schema var gets
    /// `Slot::Unbound` there. Used where an operator's output is born as
    /// decoded terms (the path-closure endpoints).
    pub fn from_bindings(rows: Vec<Bindings>) -> Self {
        use std::collections::BTreeSet;
        let mut names: BTreeSet<String> = BTreeSet::new();
        for b in &rows {
            for k in b.keys() {
                names.insert(k.to_owned());
            }
        }
        let schema: Vec<Var> = names.iter().map(|n| Var::new(n.as_str())).collect();
        let out_rows = rows
            .iter()
            .map(|b| {
                Row(schema
                    .iter()
                    .map(|v| match b.get(v.name()) {
                        Some(t) => Slot::Term(t.clone()),
                        None => Slot::Unbound,
                    })
                    .collect())
            })
            .collect();
        Batch {
            schema,
            rows: out_rows,
        }
    }

    /// Decode every row to a `Bindings`, the result-boundary step. `Unbound`
    /// slots contribute no key (matching today's "var simply absent"). `Id`
    /// slots go through `decode`; `Term` slots clone.
    pub fn to_bindings(&self, decode: impl Fn(TermId) -> Result<Term>) -> Result<Vec<Bindings>> {
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut b = Bindings::new();
            for (i, slot) in row.0.iter().enumerate() {
                match slot {
                    Slot::Id(id) => b.set(self.schema[i].name().to_owned(), decode(*id)?),
                    Slot::Term(t) => b.set(self.schema[i].name().to_owned(), t.clone()),
                    Slot::Unbound => {}
                }
            }
            out.push(b);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Term;
    use horndb_storage::TermId;

    // Fake resolver: decode id N to Iri "t{N}".
    fn decode(id: TermId) -> crate::error::Result<Term> {
        Ok(Term::Iri(format!("t{}", id.0)))
    }

    #[test]
    fn id_equals_id_by_id_no_decode() {
        // Same id → equal; different id → not equal. Decode must NOT be called
        // (pass a panicking resolver to prove it).
        let panic = |_: TermId| -> crate::error::Result<Term> { panic!("decoded") };
        assert!(Slot::eq(&Slot::Id(TermId(5)), &Slot::Id(TermId(5)), panic).unwrap());
        assert!(!Slot::eq(&Slot::Id(TermId(5)), &Slot::Id(TermId(6)), panic).unwrap());
    }

    #[test]
    fn id_vs_term_compares_by_decoded_term() {
        // Id(5) decodes to Iri("t5"); equal to Term(Iri("t5")), unequal to Term(Iri("x")).
        assert!(Slot::eq(
            &Slot::Id(TermId(5)),
            &Slot::Term(Term::Iri("t5".into())),
            decode
        )
        .unwrap());
        assert!(!Slot::eq(
            &Slot::Id(TermId(5)),
            &Slot::Term(Term::Iri("x".into())),
            decode
        )
        .unwrap());
    }

    #[test]
    fn term_vs_term_and_unbound() {
        assert!(Slot::eq(
            &Slot::Term(Term::Iri("a".into())),
            &Slot::Term(Term::Iri("a".into())),
            decode
        )
        .unwrap());
        assert!(!Slot::eq(
            &Slot::Term(Term::Iri("a".into())),
            &Slot::Term(Term::Iri("b".into())),
            decode
        )
        .unwrap());
        // Unbound equals only Unbound.
        assert!(Slot::eq(&Slot::Unbound, &Slot::Unbound, decode).unwrap());
        // Prove the Unbound short-circuit never touches the decoder.
        let panic = |_: TermId| -> crate::error::Result<Term> { panic!("decoded") };
        assert!(!Slot::eq(&Slot::Unbound, &Slot::Id(TermId(5)), panic).unwrap());
    }

    #[test]
    fn from_bindings_uneven_rows_fill_unbound() {
        use crate::exec::Bindings;
        let mut b1 = Bindings::new();
        b1.set("s", Term::Iri("http://a".into()));
        let mut b2 = Bindings::new();
        b2.set("o", Term::Literal("\"1\"".into()));
        let batch = Batch::from_bindings(vec![b1, b2]);
        // Schema is the sorted union: ["o", "s"]
        assert_eq!(
            batch.schema.iter().map(|v| v.name()).collect::<Vec<_>>(),
            ["o", "s"]
        );
        // Row 0 binds only s: o=Unbound, s=Term(...)
        assert_eq!(batch.rows[0].0[0], Slot::Unbound);
        assert!(matches!(batch.rows[0].0[1], Slot::Term(_)));
        // Row 1 binds only o: o=Term(...), s=Unbound
        assert!(matches!(batch.rows[1].0[0], Slot::Term(_)));
        assert_eq!(batch.rows[1].0[1], Slot::Unbound);
    }

    #[test]
    fn from_then_to_bindings_roundtrips() {
        use crate::exec::Bindings;
        let mut b = Bindings::new();
        b.set("s", Term::Iri("http://x".into()));
        b.set("o", Term::Literal("\"1\"".into()));
        let batch = Batch::from_bindings(vec![b.clone()]);
        let back = batch.to_bindings(decode).unwrap();
        assert_eq!(back, vec![b]);
    }
}
