//! Correctness gates for the parallel-chunked bulk loaders (HDB-83).
//!
//! Every test here answers the same question in a different shape: does
//! splitting a document across N parser threads produce exactly the store a
//! single-threaded parse of the same bytes produces?
//!
//! "Exactly" is meant literally. Interning runs on the calling thread in
//! document order in both paths, so the two stores must agree on their term
//! ids as well as their triples — which is what [`assert_same_store`] checks.
//!
//! The `*_with_threads` entry points bypass the size floor that
//! `load_*_slice` applies, so these documents can stay small enough to keep
//! the suite fast while still being split (`oxttl` needs ~16 KiB per chunk).

use horndb_storage::loader::nquads::{load_nquads_reader, load_nquads_slice_with_threads};
use horndb_storage::loader::ntriples::{load_ntriples_reader, load_ntriples_slice_with_threads};
use horndb_storage::loader::turtle::{
    load_turtle_reader_with_base, load_turtle_slice_with_threads, turtle_split_is_safe,
};
use horndb_storage::loader::{
    load_buffer_triples, set_load_batch_triples, set_load_buffer_triples,
    DEFAULT_LOAD_BATCH_TRIPLES, DEFAULT_LOAD_BUFFER_TRIPLES,
};
use horndb_storage::{Store, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};

const THREADS: usize = 8;

/// Assert two stores are indistinguishable: same triple count, same dictionary
/// size, same interned term ids, and same decoded triples per graph.
fn assert_same_store(serial: &Store, parallel: &Store, what: &str) {
    assert_eq!(
        serial.triple_count(),
        parallel.triple_count(),
        "{what}: triple count"
    );
    assert_eq!(
        serial.dictionary().len(),
        parallel.dictionary().len(),
        "{what}: dictionary size"
    );

    // Term ids, not just terms: document-order interning must assign the same
    // id to the same term in both paths.
    let mut a = serial.scan_all_term_ids();
    let mut b = parallel.scan_all_term_ids();
    a.sort_unstable_by_key(|t| (t.0.bits(), t.1.bits(), t.2.bits()));
    b.sort_unstable_by_key(|t| (t.0.bits(), t.1.bits(), t.2.bits()));
    assert_eq!(a, b, "{what}: interned term ids");

    // Decoded triples, per graph.
    let (sa, sb) = (serial.snapshot(), parallel.snapshot());
    let mut ga = sa.graphs();
    let mut gb = sb.graphs();
    ga.sort_unstable_by_key(|g| g.0);
    gb.sort_unstable_by_key(|g| g.0);
    assert_eq!(ga.len(), gb.len(), "{what}: graph count");
    for (g1, g2) in ga.iter().zip(gb.iter()) {
        let mut t1 = sa.scan_graph(*g1).unwrap();
        let mut t2 = sb.scan_graph(*g2).unwrap();
        t1.sort_unstable_by_key(|t| format!("{t:?}"));
        t2.sort_unstable_by_key(|t| format!("{t:?}"));
        assert_eq!(t1, t2, "{what}: graph contents");
    }
}

/// Answer the same "query" (a predicate scan) against both stores.
fn assert_same_answers(serial: &Store, parallel: &Store, predicate: &str) {
    let p = Term::NamedNode(NamedNode::new(predicate).unwrap());
    let mut a = serial.scan_predicate(DEFAULT_GRAPH, &p).unwrap();
    let mut b = parallel.scan_predicate(DEFAULT_GRAPH, &p).unwrap();
    assert!(!a.is_empty(), "{predicate}: expected some answers");
    a.sort_unstable_by_key(|t| format!("{t:?}"));
    b.sort_unstable_by_key(|t| format!("{t:?}"));
    assert_eq!(a, b, "{predicate}: query answers");
}

/// ~1 MB of N-Triples exercising the shapes that could break a byte-offset
/// split: labelled blank nodes, escaped newlines and quotes inside literals,
/// `#` inside a literal (a comment marker that is not a comment), a dot
/// inside a literal and inside an IRI, and language/datatype tags.
fn ntriples_corpus(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "<http://ex.org/s{i}> <http://ex.org/p.dotted> <http://ex.org/o{i}#frag> .\n"
        ));
        s.push_str(&format!(
            "<http://ex.org/s{i}> <http://ex.org/label> \"line{i}\\nnext \\\"quoted\\\" # not a comment .\"@en .\n"
        ));
        s.push_str(&format!(
            "_:node{i} <http://ex.org/about> <http://ex.org/s{i}> .\n"
        ));
        s.push_str(&format!(
            "<http://ex.org/s{i}> <http://ex.org/n> \"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n"
        ));
    }
    s
}

fn nquads_corpus(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let g = i % 4;
        s.push_str(&format!(
            "<http://ex.org/s{i}> <http://ex.org/p> \"v{i} . # \" <http://ex.org/g{g}> .\n"
        ));
        s.push_str(&format!(
            "_:node{i} <http://ex.org/about> <http://ex.org/s{i}> .\n"
        ));
    }
    s
}

#[test]
fn ntriples_parallel_matches_serial() {
    let doc = ntriples_corpus(6_000);
    assert!(doc.len() > 1 << 20, "corpus must be big enough to split");

    let serial = Store::in_memory();
    load_ntriples_reader(&serial, doc.as_bytes()).unwrap();

    let parallel = Store::in_memory();
    let stats = load_ntriples_slice_with_threads(&parallel, doc.as_bytes(), THREADS).unwrap();

    assert_eq!(stats.triples, 24_000);
    assert_same_store(&serial, &parallel, "n-triples");
    assert_same_answers(&serial, &parallel, "http://ex.org/label");
    assert_same_answers(&serial, &parallel, "http://ex.org/about");
}

#[test]
fn nquads_parallel_matches_serial() {
    let doc = nquads_corpus(12_000);
    assert!(doc.len() > 1 << 20, "corpus must be big enough to split");

    let serial = Store::in_memory();
    load_nquads_reader(&serial, doc.as_bytes()).unwrap();

    let parallel = Store::in_memory();
    let stats = load_nquads_slice_with_threads(&parallel, doc.as_bytes(), THREADS).unwrap();

    assert_eq!(stats.triples, 24_000);
    assert_same_store(&serial, &parallel, "n-quads");
}

/// Gate 3: a labelled blank node used at both ends of the document lands in
/// different chunks. `oxttl` emits labelled blank nodes verbatim
/// (`BlankNode::new_unchecked`), so document scope survives the split — this
/// pins that behaviour, since a parser that renamed per chunk would silently
/// split one node into N.
#[test]
fn blank_node_spanning_chunk_boundary_stays_one_node() {
    let mut doc = String::new();
    doc.push_str("_:shared <http://ex.org/p> <http://ex.org/first> .\n");
    // Filler wide enough that the shared node's two mentions cannot share a
    // chunk at eight-way parallelism.
    for i in 0..20_000 {
        doc.push_str(&format!(
            "<http://ex.org/filler{i}> <http://ex.org/p> <http://ex.org/o{i}> .\n"
        ));
        // Every filler line also references the shared node, so the node is
        // live in every chunk, not just the first and last.
        if i % 1_000 == 0 {
            doc.push_str(&format!("_:shared <http://ex.org/seen> \"{i}\" .\n"));
        }
    }
    doc.push_str("_:shared <http://ex.org/p> <http://ex.org/last> .\n");
    assert!(doc.len() > 1 << 20);

    let serial = Store::in_memory();
    load_ntriples_reader(&serial, doc.as_bytes()).unwrap();
    let parallel = Store::in_memory();
    load_ntriples_slice_with_threads(&parallel, doc.as_bytes(), THREADS).unwrap();

    assert_same_store(&serial, &parallel, "blank node across chunks");

    // The shared blank node must be a single dictionary entry carrying all 22
    // of its statements (2 endpoints + 20 `seen` markers).
    let bnode = Term::BlankNode(oxrdf::BlankNode::new("shared").unwrap());
    let id = parallel
        .dictionary()
        .get(&bnode)
        .expect("_:shared must be interned exactly once");
    let mentions = parallel
        .scan_all_term_ids()
        .into_iter()
        .filter(|(s, _, _)| *s == id)
        .count();
    assert_eq!(mentions, 22, "all mentions must resolve to the same node");
}

/// Turtle whose prefixes are all declared up front: safe to split, and the
/// split must reproduce the serial parse.
#[test]
fn turtle_parallel_matches_serial_when_prefixes_lead() {
    // The base comes from the caller, not from a `@base` directive: a
    // caller-supplied base is set on the parser before the split, so every
    // chunk inherits it (a document `@base` would not — see
    // `turtle_leading_base_directive_falls_back_to_serial`).
    let base = Some("http://base.example/");
    let mut doc = String::from(
        "@prefix ex: <http://ex.org/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..12_000 {
        doc.push_str(&format!(
            "ex:s{i} ex:p ex:o{i} ;\n  ex:label \"v{i} . @prefix not: <a> . # nope\"@en ;\n  ex:n \"{i}\"^^xsd:integer .\n"
        ));
        doc.push_str(&format!("<rel{i}> ex:about ex:s{i} .\n"));
    }
    assert!(doc.len() > 1 << 20);
    assert!(
        turtle_split_is_safe(doc.as_bytes()),
        "directives inside literals and comments must not trip the guard"
    );

    let serial = Store::in_memory();
    load_turtle_reader_with_base(&serial, doc.as_bytes(), base).unwrap();
    let parallel = Store::in_memory();
    load_turtle_slice_with_threads(&parallel, doc.as_bytes(), base, THREADS).unwrap();

    assert_same_store(&serial, &parallel, "turtle, leading prefixes");
    assert_same_answers(&serial, &parallel, "http://ex.org/label");
}

/// Gate 4a: a `@prefix` introduced part-way through the document. Splitting
/// here is what `oxttl` documents as unsound, so the guard must reject it and
/// the load must fall back to a serial parse of the same bytes.
#[test]
fn turtle_mid_document_prefix_falls_back_to_serial() {
    let mut doc = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..24_000 {
        doc.push_str(&format!("ex:subject{i} ex:predicate ex:object{i} .\n"));
    }
    doc.push_str("@prefix late: <http://late.example/> .\n");
    for i in 0..24_000 {
        doc.push_str(&format!(
            "late:subject{i} late:predicate late:object{i} .\n"
        ));
    }
    assert!(doc.len() > 1 << 20);
    assert!(
        !turtle_split_is_safe(doc.as_bytes()),
        "a mid-document @prefix must be rejected"
    );

    let serial = Store::in_memory();
    load_turtle_reader_with_base(&serial, doc.as_bytes(), None).unwrap();
    let parallel = Store::in_memory();
    load_turtle_slice_with_threads(&parallel, doc.as_bytes(), None, THREADS).unwrap();

    assert_eq!(parallel.triple_count(), 48_000);
    assert_same_store(&serial, &parallel, "turtle, mid-document @prefix");
    // The late prefix really did resolve — a wrong parse would have produced
    // `late:` IRIs against the wrong namespace, or no triples at all.
    let late = Term::NamedNode(NamedNode::new("http://late.example/predicate").unwrap());
    assert_eq!(
        parallel.scan_predicate(DEFAULT_GRAPH, &late).unwrap().len(),
        24_000
    );
}

/// Gate 4b: the same for a mid-document `@base`, which silently changes how
/// every following relative IRI resolves.
#[test]
fn turtle_mid_document_base_falls_back_to_serial() {
    let mut doc = String::from("@base <http://first.example/> .\n@prefix ex: <http://ex.org/> .\n");
    for i in 0..24_000 {
        doc.push_str(&format!("<subject{i}> ex:predicate ex:object{i} .\n"));
    }
    doc.push_str("@base <http://second.example/> .\n");
    for i in 0..24_000 {
        doc.push_str(&format!("<subject{i}> ex:other ex:object{i} .\n"));
    }
    assert!(doc.len() > 1 << 20);
    assert!(
        !turtle_split_is_safe(doc.as_bytes()),
        "a mid-document @base must be rejected"
    );

    let serial = Store::in_memory();
    load_turtle_reader_with_base(&serial, doc.as_bytes(), None).unwrap();
    let parallel = Store::in_memory();
    load_turtle_slice_with_threads(&parallel, doc.as_bytes(), None, THREADS).unwrap();

    assert_same_store(&serial, &parallel, "turtle, mid-document @base");
    // The rebased subjects exist under the *second* base, proving the fallback
    // parse honoured the directive rather than reusing the first base.
    let second = Term::NamedNode(NamedNode::new("http://second.example/subject0").unwrap());
    assert!(parallel.dictionary().get(&second).is_some());
}

/// A `@base` in the *leading* block is rejected too. `oxttl`'s chunker copies
/// the document's leading prefixes into every chunk parser but not its base,
/// so a chunk starting after the directive would resolve relative IRIs against
/// the wrong base — or, with no caller base, fail outright. The load must
/// still succeed, via the serial fallback.
#[test]
fn turtle_leading_base_directive_falls_back_to_serial() {
    let mut doc = String::from("@base <http://doc.example/> .\n@prefix ex: <http://ex.org/> .\n");
    for i in 0..24_000 {
        doc.push_str(&format!("<subject{i}> ex:predicate ex:object{i} .\n"));
    }
    assert!(doc.len() > 1 << 20);
    assert!(
        !turtle_split_is_safe(doc.as_bytes()),
        "oxttl does not propagate @base into chunks"
    );

    let serial = Store::in_memory();
    load_turtle_reader_with_base(&serial, doc.as_bytes(), None).unwrap();
    let parallel = Store::in_memory();
    load_turtle_slice_with_threads(&parallel, doc.as_bytes(), None, THREADS).unwrap();

    assert_eq!(parallel.triple_count(), 24_000);
    assert_same_store(&serial, &parallel, "turtle, leading @base");
    let rebased = Term::NamedNode(NamedNode::new("http://doc.example/subject0").unwrap());
    assert!(parallel.dictionary().get(&rebased).is_some());
}

/// SPARQL-style `PREFIX` / `BASE` (no `@`, no terminating dot) are directives
/// too, and Turtle 1.2 allows them anywhere a `@`-form is allowed.
#[test]
fn turtle_guard_rejects_mid_document_sparql_directives() {
    let head = "PREFIX ex: <http://ex.org/>\n";
    let body = "ex:s ex:p ex:o .\n";
    assert!(turtle_split_is_safe(format!("{head}{body}").as_bytes()));
    // A leading SPARQL-style BASE is rejected for the same reason `@base` is.
    assert!(!turtle_split_is_safe(
        format!("BASE <http://base.example/>\n{head}{body}").as_bytes()
    ));
    assert!(!turtle_split_is_safe(
        format!("{head}{body}PREFIX late: <http://late.example/>\nlate:s late:p late:o .\n")
            .as_bytes()
    ));
    assert!(!turtle_split_is_safe(
        format!("{head}{body}BASE <http://other.example/>\n<s> ex:p ex:o .\n").as_bytes()
    ));
}

/// The guard must not fire on directive-looking text that is not a directive:
/// inside literals (short and long), inside comments, inside IRIs, or as part
/// of a longer name.
#[test]
fn turtle_guard_ignores_directive_lookalikes() {
    let cases = [
        "@prefix ex: <http://ex.org/> .\nex:s ex:p \"@prefix a: <b> .\" .\n",
        "@prefix ex: <http://ex.org/> .\nex:s ex:p \"\"\"multi\n@base <x>\nline\"\"\" .\n",
        "@prefix ex: <http://ex.org/> .\nex:s ex:p ex:o . # @prefix a: <b> .\n",
        "@prefix ex: <http://ex.org/> .\nex:s ex:p <http://ex.org/@prefix> .\n",
        "@prefix ex: <http://ex.org/> .\nex:s ex:p ex:prefixed , ex:basement .\n",
        "@prefix ex: <http://ex.org/> .\nex:prefix ex:base ex:o .\n",
        "@prefix ex: <http://ex.org/> .\nex:s ex:p 'single @base <x> .' .\n",
    ];
    for c in cases {
        assert!(
            turtle_split_is_safe(c.as_bytes()),
            "false positive on:\n{c}"
        );
    }
}

/// A document with no directives at all is splittable.
#[test]
fn turtle_guard_accepts_directive_free_document() {
    assert!(turtle_split_is_safe(
        b"<http://ex.org/s> <http://ex.org/p> <http://ex.org/o> .\n"
    ));
    assert!(turtle_split_is_safe(b""));
}

/// Gate 5: the streaming `Read` path is untouched and still serves sources
/// that cannot be sliced. `NonSeekable` implements only `Read`, so it can only
/// go through the streaming loader.
#[test]
fn streaming_reader_path_still_works() {
    struct NonSeekable<'a>(&'a [u8]);
    impl std::io::Read for NonSeekable<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // Hand back one byte at a time: a stream, not a buffer.
            if self.0.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    let doc = "<http://ex.org/s> <http://ex.org/p> <http://ex.org/o> .\n";
    let store = Store::in_memory();
    let stats = load_ntriples_reader(&store, NonSeekable(doc.as_bytes())).unwrap();
    assert_eq!(stats.triples, 1);
    assert_eq!(store.triple_count(), 1);

    let ttl = "@prefix ex: <http://ex.org/> .\nex:s ex:p ex:o .\n";
    let store = Store::in_memory();
    load_turtle_reader_with_base(&store, NonSeekable(ttl.as_bytes()), None).unwrap();
    assert_eq!(store.triple_count(), 1);
}

/// A parse error in any chunk must surface, not be swallowed by the pipeline.
#[test]
fn parse_error_in_a_late_chunk_surfaces() {
    let mut doc = String::new();
    for i in 0..20_000 {
        doc.push_str(&format!(
            "<http://ex.org/s{i}> <http://ex.org/p> <http://ex.org/o{i}> .\n"
        ));
    }
    doc.push_str("this is not n-triples\n");
    assert!(doc.len() > 1 << 20);

    let store = Store::in_memory();
    let err = load_ntriples_slice_with_threads(&store, doc.as_bytes(), THREADS).unwrap_err();
    assert!(
        matches!(err, horndb_storage::StorageError::NtriplesParse(_)),
        "unexpected error: {err:?}"
    );
}

/// Acceptance gate 2, swept over the checked-in corpora rather than a
/// synthetic document: for every fixture in the workspace, the parallel entry
/// point must produce the same store as the streaming one.
///
/// Most fixtures are a few hundred bytes, and `oxttl` clamps to a single chunk
/// below 16 KiB, so for those this compares the fallback. To get a real split
/// on real data, each line-based corpus is also replicated up past 1 MiB —
/// legal for N-Triples/N-Quads, and it keeps the triple set identical.
#[test]
fn every_fixture_corpus_loads_identically() {
    fn fixtures(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                fixtures(&p, out);
            } else if matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("ttl") | Some("nt") | Some("nq")
            ) {
                out.push(p);
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut corpora = Vec::new();
    fixtures(&root.join("crates/storage/tests/fixtures"), &mut corpora);
    fixtures(&root.join("crates/harness/tests/fixtures"), &mut corpora);
    corpora.sort();
    assert!(
        corpora.len() > 20,
        "expected a corpus sweep, not a spot check"
    );

    let mut compared = 0usize;
    for path in corpora {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let ext = path.extension().and_then(|s| s.to_str()).unwrap();
        let name = path.display().to_string();

        // A fixture that the serial parser rejects (there are deliberate bad
        // ones) must be rejected by the parallel entry point too.
        let serial = Store::in_memory();
        let parallel = Store::in_memory();
        let (a, b) = match ext {
            "nt" => (
                load_ntriples_reader(&serial, bytes.as_slice()),
                load_ntriples_slice_with_threads(&parallel, &bytes, THREADS),
            ),
            "nq" => (
                load_nquads_reader(&serial, bytes.as_slice()),
                load_nquads_slice_with_threads(&parallel, &bytes, THREADS),
            ),
            _ => (
                load_turtle_reader_with_base(&serial, bytes.as_slice(), None),
                load_turtle_slice_with_threads(&parallel, &bytes, None, THREADS),
            ),
        };
        assert_eq!(a.is_ok(), b.is_ok(), "{name}: parse outcome differs");
        if a.is_err() {
            continue;
        }
        assert_same_store(&serial, &parallel, &name);
        compared += 1;

        // Line-based corpora: replicate past the chunk floor so the split is
        // genuinely exercised on real data.
        if ext == "nt" || ext == "nq" {
            if bytes.is_empty() {
                continue;
            }
            let reps = ((1 << 20) / bytes.len()) + 2;
            let mut big = Vec::with_capacity(bytes.len() * reps);
            for _ in 0..reps {
                big.extend_from_slice(&bytes);
                if big.last() != Some(&b'\n') {
                    big.push(b'\n');
                }
            }
            let serial = Store::in_memory();
            let parallel = Store::in_memory();
            if ext == "nt" {
                load_ntriples_reader(&serial, big.as_slice()).unwrap();
                load_ntriples_slice_with_threads(&parallel, &big, THREADS).unwrap();
            } else {
                load_nquads_reader(&serial, big.as_slice()).unwrap();
                load_nquads_slice_with_threads(&parallel, &big, THREADS).unwrap();
            }
            assert_same_store(&serial, &parallel, &format!("{name} (replicated)"));
        }
    }
    assert!(compared > 20, "only {compared} corpora compared");
}

/// The in-flight buffer bound (HDB-94) is a throughput knob, never a
/// correctness one: it changes only how far a parse thread may run ahead of
/// the document-order drain, so every budget must produce the same store.
///
/// The budgets below are chosen to land on per-chunk depths 1, 2, 7 and 64 at
/// `THREADS` chunks, including the pre-HDB-94 default (2 batches per chunk)
/// and a budget so small it clamps to one batch.
#[test]
fn buffer_budget_does_not_change_the_store() {
    const PER_CHUNK_BATCH: usize = 8_192 * THREADS;
    let doc = ntriples_corpus(6_000);
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..48_000 {
        ttl.push_str(&format!("ex:subject{i} ex:predicate ex:object{i} .\n"));
    }
    assert!(ttl.len() > 1 << 20);
    assert!(turtle_split_is_safe(ttl.as_bytes()));

    let serial = Store::in_memory();
    load_ntriples_reader(&serial, doc.as_bytes()).unwrap();
    let serial_ttl = Store::in_memory();
    load_turtle_reader_with_base(&serial_ttl, ttl.as_bytes(), None).unwrap();

    for budget in [
        1,
        2 * PER_CHUNK_BATCH,
        7 * PER_CHUNK_BATCH,
        64 * PER_CHUNK_BATCH,
        DEFAULT_LOAD_BUFFER_TRIPLES,
    ] {
        set_load_buffer_triples(budget);
        assert!(load_buffer_triples() >= 1);

        let parallel = Store::in_memory();
        load_ntriples_slice_with_threads(&parallel, doc.as_bytes(), THREADS).unwrap();
        assert_same_store(&serial, &parallel, &format!("n-triples @ budget {budget}"));

        let parallel_ttl = Store::in_memory();
        load_turtle_slice_with_threads(&parallel_ttl, ttl.as_bytes(), None, THREADS).unwrap();
        assert_same_store(
            &serial_ttl,
            &parallel_ttl,
            &format!("turtle @ budget {budget}"),
        );
    }

    set_load_buffer_triples(DEFAULT_LOAD_BUFFER_TRIPLES);
}

/// The tier batch size (HDB-84) is a throughput knob, never a correctness one.
/// The tier now appends each batch as its own run and merges the runs on first
/// read, so a document split into many small inserts must produce exactly the
/// store one big insert produces — same triples, same dictionary, same term
/// ids.
#[test]
fn batch_size_does_not_change_the_store() {
    let doc = ntriples_corpus(6_000);

    set_load_batch_triples(DEFAULT_LOAD_BATCH_TRIPLES);
    let one_shot = Store::in_memory();
    load_ntriples_reader(&one_shot, doc.as_bytes()).unwrap();

    for batch in [1usize, 7, 1_024, 8_192, 1 << 20] {
        set_load_batch_triples(batch);

        let streamed = Store::in_memory();
        load_ntriples_reader(&streamed, doc.as_bytes()).unwrap();
        assert_same_store(&one_shot, &streamed, &format!("streamed @ batch {batch}"));

        let parallel = Store::in_memory();
        load_ntriples_slice_with_threads(&parallel, doc.as_bytes(), THREADS).unwrap();
        assert_same_store(&one_shot, &parallel, &format!("parallel @ batch {batch}"));
    }

    set_load_batch_triples(DEFAULT_LOAD_BATCH_TRIPLES);
}

/// The HDB-106 probe gate must not be visible in the store.
///
/// `loader::parallel::should_probe` turns the parse-thread dictionary probe off
/// below 4 chunks, because there it costs 4–5% instead of saving 8% (the
/// measurements are on the constant). That is a pure throughput decision: both
/// sides of the gate have to produce the same triples, the same dictionary and
/// **the same term ids** as a serial reader, or the gate has become a
/// correctness switch.
///
/// Thread counts are chosen to straddle it: 1 and 2 fall below (unprobed), 4
/// and 8 at or above (probed). `oxttl`'s 16 KiB-per-chunk floor means the
/// document has to be big enough to actually split that many ways, which
/// `ntriples_corpus(6_000)` is.
#[test]
fn both_sides_of_the_probe_gate_produce_the_same_store() {
    let doc = ntriples_corpus(6_000);
    assert!(doc.len() > 1 << 20, "corpus must be big enough to split");

    let serial = Store::in_memory();
    load_ntriples_reader(&serial, doc.as_bytes()).unwrap();

    for threads in [1usize, 2, 3, 4, 8] {
        let parallel = Store::in_memory();
        load_ntriples_slice_with_threads(&parallel, doc.as_bytes(), threads).unwrap();
        assert_same_store(
            &serial,
            &parallel,
            &format!("n-triples @ {threads} threads"),
        );
    }
}

/// The same, for N-Quads — the format whose graph label is resolved on the
/// consumer rather than probed, so the gate crosses a second code path.
#[test]
fn both_sides_of_the_probe_gate_agree_on_named_graphs() {
    let doc = nquads_corpus(12_000);
    assert!(doc.len() > 1 << 20, "corpus must be big enough to split");

    let serial = Store::in_memory();
    load_nquads_reader(&serial, doc.as_bytes()).unwrap();

    for threads in [1usize, 2, 3, 4, 8] {
        let parallel = Store::in_memory();
        load_nquads_slice_with_threads(&parallel, doc.as_bytes(), threads).unwrap();
        assert_same_store(&serial, &parallel, &format!("n-quads @ {threads} threads"));
    }
}
