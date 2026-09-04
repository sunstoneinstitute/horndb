//! SPEC-25 S2 / acceptance #2: a dictionary flushed to disk and reopened
//! resolves id → term and term → id without re-interning, keeps every
//! `GraphId`, reloads reclaimed indices as reclaimed, and hands the next new
//! term the id it would have got without the restart.

use horndb_storage::loader::nquads::{load_nquads_reader, load_nquads_slice_with_threads};
use horndb_storage::{Dictionary, Store, TermId, TermKind};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use std::fmt::Write as _;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn typed(lex: &str, dt: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(lex, NamedNode::new(dt).unwrap()))
}

/// One term of every dictionary-allocated kind.
fn every_kind() -> Vec<Term> {
    vec![
        iri("http://ex/a"),
        Term::BlankNode(BlankNode::new("b0").unwrap()),
        Term::Literal(Literal::new_simple_literal("plain")),
        Term::Literal(Literal::new_language_tagged_literal("colour", "en-GB").unwrap()),
        typed("1.5", "http://www.w3.org/2001/XMLSchema#decimal"),
        typed("042", "http://www.w3.org/2001/XMLSchema#integer"), // non-canonical: not inlined
        Term::Literal(
            Literal::new_directional_language_tagged_literal(
                "שלום",
                "he",
                oxrdf::BaseDirection::Rtl,
            )
            .unwrap(),
        ),
        Term::Triple(Box::new(Triple {
            subject: NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/s").unwrap()),
            predicate: NamedNode::new("http://ex/p").unwrap(),
            object: typed("7", "http://ex/dt"),
        })),
    ]
}

#[test]
fn reopen_resolves_both_directions_and_continues_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dict.base");
    let terms = every_kind();

    let fresh = Dictionary::new();
    let ids: Vec<TermId> = terms.iter().map(|t| fresh.intern(t).unwrap()).collect();
    let stats = fresh.flush(&path).unwrap();
    assert_eq!(stats.slots as usize, terms.len());
    assert_eq!(stats.freed, 0);
    drop(fresh);

    let reopened = Dictionary::open(&path).unwrap();
    assert_eq!(reopened.len(), terms.len());
    assert_eq!(reopened.live_len(), terms.len());
    assert_eq!(reopened.base_len(), terms.len());
    for (term, id) in terms.iter().zip(&ids) {
        assert_eq!(
            reopened.lookup(*id).as_ref(),
            Some(term),
            "id -> term {term}"
        );
        // `get` is the read-only probe the parse threads use (HDB-106); the
        // typed and language-tagged terms exercise the case where this
        // process has never seen the datatype IRI / tag.
        assert_eq!(reopened.get(term), Some(*id), "term -> id {term}");
        // Interning a base term allocates nothing.
        assert_eq!(reopened.intern(term).unwrap(), *id);
    }
    assert_eq!(
        reopened.len(),
        terms.len(),
        "re-interning must not allocate"
    );
    assert_eq!(
        reopened.lookup_batch(&ids),
        terms.iter().cloned().map(Some).collect::<Vec<_>>()
    );
    assert_eq!(reopened.numeric_value(ids[4]), Some(1.5));
    assert_eq!(reopened.get(&iri("http://ex/never")), None);

    // A new term continues the index space exactly where the file stopped.
    let new_id = reopened.intern(&iri("http://ex/new")).unwrap();
    assert_eq!(new_id, TermId::new(TermKind::Uri, terms.len() as u64 + 1));
    assert_eq!(reopened.lookup(new_id), Some(iri("http://ex/new")));
    assert_eq!(reopened.len(), terms.len() + 1);

    // Second generation: base + overlay merge into one file that reopens.
    let path2 = dir.path().join("dict2.base");
    let stats = reopened.flush(&path2).unwrap();
    assert_eq!(stats.slots as usize, terms.len() + 1);
    let third = Dictionary::open(&path2).unwrap();
    assert_eq!(third.len(), terms.len() + 1);
    for (term, id) in terms.iter().zip(&ids) {
        assert_eq!(third.get(term), Some(*id));
        assert_eq!(third.lookup(*id).as_ref(), Some(term));
    }
    assert_eq!(third.get(&iri("http://ex/new")), Some(new_id));
}

#[test]
fn reopen_keeps_graph_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dict.base");
    let g = iri("http://ex/graph/1");
    let store = Store::in_memory();
    let gid = store.intern_graph_uri(&g).unwrap();
    store.dictionary().flush(&path).unwrap();
    drop(store);

    let store = Store::with_dictionary(Dictionary::open(&path).unwrap());
    assert_eq!(store.intern_graph_uri(&g).unwrap(), gid);
    assert_eq!(store.dictionary().len(), 1);
}

#[test]
fn reclaimed_indices_reload_as_tombstones_and_are_not_reissued() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dict.base");
    let keep = (
        iri("http://ex/s"),
        iri("http://ex/p"),
        iri("http://ex/keep"),
    );
    let gone = (
        iri("http://ex/s"),
        iri("http://ex/p"),
        iri("http://ex/gone"),
    );

    let store = Store::in_memory();
    store.insert_triples(&[keep.clone(), gone.clone()]).unwrap();
    let gone_id = store.dictionary().get(&gone.2).unwrap();
    store.retract_triples(std::slice::from_ref(&gone)).unwrap();
    store.compact();
    assert_eq!(store.dictionary().lookup(gone_id), None);
    let stats = store.dictionary().flush(&path).unwrap();
    assert_eq!((stats.slots, stats.freed), (4, 1));
    drop(store);

    let store = Store::with_dictionary(Dictionary::open(&path).unwrap());
    let d = store.dictionary();
    assert_eq!((d.len(), d.live_len()), (4, 3));
    assert_eq!(d.lookup(gone_id), None, "tombstone reloads as reclaimed");
    assert_eq!(d.get(&gone.2), None);
    // The freed index is not re-issued: the term comes back under a new one.
    let again = d.intern(&gone.2).unwrap();
    assert_eq!(again.payload(), 5);
    assert_eq!(d.lookup(again), Some(gone.2.clone()));

    // GC of a *base* term after reopen: the file is immutable, so the index
    // is recorded dead, resolves to nothing in both directions, and the next
    // flush writes the tombstone.
    store.insert_triples(std::slice::from_ref(&keep)).unwrap();
    let keep_id = d.get(&keep.2).unwrap();
    assert!(keep_id.payload() <= 4, "keep must be a base term");
    store.retract_triples(std::slice::from_ref(&keep)).unwrap();
    store.compact();
    assert_eq!(d.lookup(keep_id), None);
    assert_eq!(d.get(&keep.2), None);
    // Everything else is swept too — the subject, and `gone`'s re-interned
    // overlay slot — except the predicate: its now-empty partition still
    // exists, and the mark phase marks every partition key (HDB-121).
    assert_eq!(d.live_len(), 1);
    assert_eq!(d.get(&keep.1).map(|id| id.payload()), Some(2));
    let path2 = dir.path().join("dict2.base");
    let stats = d.flush(&path2).unwrap();
    assert_eq!((stats.slots, stats.freed), (5, 4));
    let third = Dictionary::open(&path2).unwrap();
    assert_eq!((third.len(), third.live_len()), (5, 1));
    assert_eq!(third.get(&keep.1).map(|id| id.payload()), Some(2));
    assert_eq!(third.lookup(keep_id), None);
    assert_eq!(third.lookup(again), None);
    assert_eq!(third.intern(&keep.2).unwrap().payload(), 6);
}

/// LUBM-shaped N-Quads with named graphs, literals of several kinds and
/// blank nodes — the mix the loaders and the dictionary see in practice.
fn corpus() -> String {
    let mut doc = String::new();
    for i in 0..2_000u32 {
        let g = if i % 3 == 0 {
            String::new()
        } else {
            format!(" <http://ex/graph/{}>", i % 3)
        };
        let s = format!(
            "<http://www.Department{}.University0.edu/Student{i}>",
            i % 7
        );
        writeln!(doc, "{s} <http://ex/name> \"Student {i}\"{g} .").unwrap();
        writeln!(
            doc,
            "{s} <http://ex/age> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>{g} .",
            18 + i % 30
        )
        .unwrap();
        writeln!(
            doc,
            "{s} <http://ex/gpa> \"{}.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>{g} .",
            i % 4
        )
        .unwrap();
        writeln!(doc, "{s} <http://ex/label> \"étudiant {i}\"@fr{g} .").unwrap();
        writeln!(doc, "{s} <http://ex/advisor> _:prof{}{g} .", i % 50).unwrap();
        writeln!(
            doc,
            "_:prof{} <http://ex/teaches> <http://ex/course/{}>{g} .",
            i % 50,
            i % 90
        )
        .unwrap();
    }
    doc
}

fn sorted_ids(store: &Store) -> Vec<(TermId, TermId, TermId)> {
    let mut v = store.scan_all_term_ids();
    v.sort_unstable_by_key(|t| (t.0.bits(), t.1.bits(), t.2.bits()));
    v
}

/// The HDB-106 differential pattern: a corpus loaded into a fresh store and
/// the same corpus loaded into a store reopened on that dictionary carry
/// identical term ids — and the reload allocates no id at all.
#[test]
fn reload_into_reopened_store_assigns_no_new_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dict.base");
    let doc = corpus();

    let fresh = Store::in_memory();
    load_nquads_reader(&fresh, doc.as_bytes()).unwrap();
    let fresh_len = fresh.dictionary().len();
    fresh.dictionary().flush(&path).unwrap();

    let reopened = Store::with_dictionary(Dictionary::open(&path).unwrap());
    assert_eq!(reopened.dictionary().len(), fresh_len);
    // Threaded, so the parse-thread `get` probes run against the base.
    load_nquads_slice_with_threads(&reopened, doc.as_bytes(), 4).unwrap();
    assert_eq!(
        reopened.dictionary().len(),
        fresh_len,
        "reload allocated ids"
    );
    assert_eq!(reopened.triple_count(), fresh.triple_count());

    assert_eq!(
        sorted_ids(&reopened),
        sorted_ids(&fresh),
        "default-graph ids"
    );
    let (sa, sb) = (fresh.snapshot(), reopened.snapshot());
    let mut ga = sa.graphs();
    let mut gb = sb.graphs();
    ga.sort_unstable_by_key(|g| g.0);
    gb.sort_unstable_by_key(|g| g.0);
    assert_eq!(ga, gb, "graph ids");
    for g in &ga {
        let mut a: Vec<_> = sa.iter_graph_term_ids(*g).collect();
        let mut b: Vec<_> = sb.iter_graph_term_ids(*g).collect();
        a.sort_unstable_by_key(|t| (t.0.bits(), t.1.bits(), t.2.bits()));
        b.sort_unstable_by_key(|t| (t.0.bits(), t.1.bits(), t.2.bits()));
        assert_eq!(a, b, "graph {g:?} ids");
        let mut ta = sa.scan_graph(*g).unwrap();
        let mut tb = sb.scan_graph(*g).unwrap();
        ta.sort_unstable_by_key(|t| format!("{t:?}"));
        tb.sort_unstable_by_key(|t| format!("{t:?}"));
        assert_eq!(ta, tb, "graph {g:?} terms");
    }
}
