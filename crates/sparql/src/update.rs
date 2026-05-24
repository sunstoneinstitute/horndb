//! SPARQL Update — Stage 1 supports only `INSERT DATA` and
//! `DELETE DATA` literal forms.
//!
//! `LOAD`, `CLEAR`, `DROP`, and template `INSERT { … } WHERE { … }` /
//! `DELETE { … } WHERE { … }` are explicitly deferred (see SPEC-07
//! Future Work). The parser still accepts them; this module
//! rejects them at apply time.

use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::Store;
use crate::parser::ParsedUpdate;
use spargebra::term::{GroundTerm, NamedOrBlankNode, Term as SpgTerm};

/// Apply an update to a [`Store`]. Returns `Ok(())` on success.
pub fn apply_update<S: Store>(u: &ParsedUpdate, store: &mut S) -> Result<()> {
    use spargebra::GraphUpdateOperation;
    let ops = match u {
        ParsedUpdate::InsertData { inner } | ParsedUpdate::DeleteData { inner } => &inner.operations,
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };
    for op in ops {
        match op {
            GraphUpdateOperation::InsertData { data } => {
                for q in data {
                    let s = subject_to_term(&q.subject);
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = object_to_term(&q.object);
                    store.insert_triple(s, p, o);
                }
            }
            GraphUpdateOperation::DeleteData { data } => {
                for q in data {
                    let s = Term::Iri(q.subject.as_str().to_owned());
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = ground_term_to_term(&q.object);
                    store.delete_triple(&s, &p, &o);
                }
            }
            other => {
                return Err(SparqlError::UnsupportedAlgebra(format!(
                    "update operation: {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn subject_to_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

fn object_to_term(t: &SpgTerm) -> Term {
    match t {
        SpgTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        SpgTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        SpgTerm::Literal(l) => Term::Literal(l.to_string()),
    }
}

fn ground_term_to_term(gt: &GroundTerm) -> Term {
    match gt {
        GroundTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTerm::Literal(l) => Term::Literal(l.to_string()),
    }
}
