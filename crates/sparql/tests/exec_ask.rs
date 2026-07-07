use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s
}

#[test]
fn ask_true_when_pattern_matches() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s ?p ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(any);
}

#[test]
fn ask_false_when_pattern_misses() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s <http://ex/missing> ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(!any);
}

mod ask_early_exit {
    use horndb_sparql::algebra::{Term, TriplePattern, Var};
    use horndb_sparql::api::{execute_query, QueryAnswer};
    use horndb_sparql::exec::{Batch, Bindings, Executor, Row, Slot};
    use horndb_storage::TermId;

    /// 5000 id-rows; decoding any id >= 4096 fails. ASK must answer from the
    /// first 4096-row chunk without draining (and decoding) the rest.
    struct DecodeFailsLate;

    impl Executor for DecodeFailsLate {
        fn scan_bgp(
            &self,
            _patterns: &[TriplePattern],
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            unreachable!("scan_bgp_ids is overridden")
        }
        fn scan_bgp_ids(&self, _patterns: &[TriplePattern]) -> horndb_sparql::Result<Batch> {
            Ok(Batch {
                schema: vec![Var::new("s"), Var::new("p"), Var::new("o")],
                rows: (0u64..5000)
                    .map(|i| {
                        Row(vec![
                            Slot::Id(TermId(i)),
                            Slot::Id(TermId(i)),
                            Slot::Id(TermId(i)),
                        ])
                    })
                    .collect(),
            })
        }
        fn decode_term(&self, id: TermId) -> horndb_sparql::Result<Term> {
            if id.0 < 4096 {
                Ok(Term::Iri(format!("http://ex/t{}", id.0)))
            } else {
                Err(horndb_sparql::SparqlError::Executor(format!(
                    "decode of {} past the first chunk",
                    id.0
                )))
            }
        }
    }

    #[test]
    fn ask_answers_from_first_chunk_without_draining() {
        let ans = execute_query("ASK { ?s ?p ?o }", &DecodeFailsLate)
            .expect("ASK must not decode rows beyond the first chunk");
        assert!(matches!(ans, QueryAnswer::Boolean(true)));
    }
}
