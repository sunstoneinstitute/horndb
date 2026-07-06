---
status: in-progress
date: 2026-07-06
scope: "Streaming SELECT results end-to-end to the HTTP layer"
---

# HTTP Streaming Results Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream SELECT results end-to-end — operator chunk → per-chunk decode → per-chunk serialization → chunked HTTP frames — so `Runtime::run`'s full `Vec<Bindings>` and the fully-serialized body `String` are never built for HTTP queries ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) remaining item 3).

**Architecture:** A new `Runtime::run_stream` returns a `BindingsStream` that pulls one operator `Batch` at a time and decodes it at the boundary; `run` collects it, so no existing consumer changes. The four SELECT serializers gain incremental header/chunk/footer forms behind a `SelectSerializer` trait. The `/query` handler runs execution+decode+serialization inside `tokio::task::spawn_blocking` (the operator tree and the `RwLockReadGuard` are `!Send` — they must stay on one thread) and ships serialized `Bytes` over a bounded mpsc channel into a hand-rolled `http_body::Body`. The first chunk is pre-buffered before headers so early errors are still a clean HTTP 400; mid-stream errors abort the chunked body (protocol-level-detectable truncation). Full design rationale: `docs/specs/SPEC-22-http-streaming-results.md`.

**Tech Stack:** Rust 1.90, axum 0.8, tokio (`spawn_blocking`, `sync::mpsc`), http-body 1, bytes — all already `server`-feature deps of `crates/sparql`. **No new dependencies.**

**Verification runner:** `cargo nextest run` (see root `CLAUDE.md`). Benchmarks only on `hornbench`.

**File map (all under `crates/sparql/` unless noted):**

| File | Change |
|---|---|
| `src/exec/runtime.rs` | add `BindingsStream` + `run_stream`; reimplement `run` on top |
| `src/exec/op/chunk_tests.rs` | new `run_stream` unit tests |
| `src/api.rs` | ASK early-exit; new `plan_select` |
| `src/results/mod.rs` | `SelectSerializer` trait + `select_serializer` factory |
| `src/results/{json,xml,csv,tsv}.rs` | incremental serializer impls; `write_select_*` become wrappers |
| `src/server/stream_body.rs` | **new** — `ChannelBody` |
| `src/server/mod.rs` | declare `stream_body` module |
| `src/server/query.rs` | streaming SELECT path |
| `tests/exec_ask.rs` | ASK early-exit test |
| `tests/results_streaming.rs` | **new** — serializer chunking-invariance tests |
| `tests/api_end_to_end.rs` | `plan_select` routing test |
| `tests/server_http.rs` | chunked-response + error-semantics tests |
| `docs/metrics.md` | `stage_duration_seconds` semantics note (same commit as the emit-site change) |
| `INTEGRATION-NOTES.md` | record the new public seams |

Do **not** touch `TASKS.md`, `docs/benchmarks.md`, `docs/architecture.md`, or `docs/index.md` — the integrating session syncs those (root `CLAUDE.md` doc-sync rule) when this branch merges.

---

### Task 1: `Runtime::run_stream` + `BindingsStream`

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs:31-40` (`run`, and new code below it)
- Test: `crates/sparql/src/exec/op/chunk_tests.rs` (append at end)

- [ ] **Step 1: Write the failing tests**

Append to `crates/sparql/src/exec/op/chunk_tests.rs` (it already has `run_sorted`, `iri`, `some_iri` helpers and the `TEST_BATCH_ROWS` thread-local at `super::TEST_BATCH_ROWS`):

```rust
// ---------------------------------------------------------------------------
// run_stream: chunked boundary decode (#128 HTTP streaming increment)
// ---------------------------------------------------------------------------

/// 7 VALUES rows at chunk size 3 → chunks of 3/3/1; concatenation must equal
/// `run`'s output, chunks must respect batch_rows() and never be empty.
#[test]
fn run_stream_chunks_match_run() {
    let horn = HornBackend::new();
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: (0u8..7).map(|i| vec![some_iri(&format!("v{i}"))]).collect(),
    };

    super::TEST_BATCH_ROWS.with(|c| c.set(3));
    let rt = Runtime::new(&horn);
    let mut stream = rt.run_stream(&plan).unwrap();
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next_chunk().unwrap() {
        assert!(!chunk.is_empty(), "no empty chunks mid-stream");
        assert!(chunk.len() <= 3, "chunk exceeds batch_rows()");
        chunks.push(chunk);
    }
    assert!(chunks.len() >= 3, "7 rows at chunk size 3 must span >=3 chunks");
    let streamed: Vec<String> = chunks.concat().iter().map(|b| format!("{b:?}")).collect();
    let collected: Vec<String> = rt.run(&plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    assert_eq!(streamed, collected);
}

/// The row-at-a-time Iterator view must yield the same rows as `run`.
#[test]
fn run_stream_iterator_matches_run() {
    let horn = HornBackend::new();
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: (0u8..7).map(|i| vec![some_iri(&format!("v{i}"))]).collect(),
    };

    super::TEST_BATCH_ROWS.with(|c| c.set(2));
    let rt = Runtime::new(&horn);
    let stream = rt.run_stream(&plan).unwrap();
    let via_iter: Vec<String> = stream.map(|r| format!("{:?}", r.unwrap())).collect();
    let via_run: Vec<String> = rt.run(&plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    assert_eq!(via_iter, via_run);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql run_stream_chunks_match_run run_stream_iterator_matches_run`
Expected: build FAILURE — `no method named run_stream found for struct Runtime`.

- [ ] **Step 3: Implement `BindingsStream` and `run_stream`; reimplement `run` on top**

In `crates/sparql/src/exec/runtime.rs`, replace the body of `run` (lines 31-40) and add the stream type directly below it:

```rust
    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let mut stream = self.run_stream(plan)?;
        let mut out = Vec::new();
        while let Some(chunk) = stream.next_chunk()? {
            out.extend(chunk);
        }
        Ok(out.into_iter())
    }

    /// Execute the plan as a stream of decoded row chunks (#128 HTTP
    /// streaming): applies the pushdown rewrite, builds the operator tree,
    /// and hands back a lazy handle. `run` collects this; the HTTP layer
    /// serializes chunk-by-chunk without ever holding the full result.
    ///
    /// The stream borrows the `Runtime` (operators hold `&Runtime`
    /// internally), so keep the runtime binding alive:
    /// `let rt = Runtime::new(exec); let mut s = rt.run_stream(&plan)?;`
    pub fn run_stream<'r>(&'r self, plan: &PhysicalPlan) -> Result<BindingsStream<'r, E>>
    where
        E: 'r,
    {
        let plan = crate::plan::pushdown::rewrite(plan)?;
        let op = self.build(&plan)?;
        Ok(BindingsStream {
            exec: self.exec,
            op,
            buf: Vec::new().into_iter(),
        })
    }
```

Then add, at module level (after the `impl Runtime` block, before `apply_filter`'s impl block ends is fine — place it right after the `impl<'a, E: Executor + ?Sized> Runtime<'a, E>` block that contains `run`):

```rust
/// Streaming query handle returned by [`Runtime::run_stream`]: pulls one
/// operator `Batch` at a time and decodes `Slot::Id → Term` at the boundary,
/// chunk-by-chunk instead of all-at-once.
pub struct BindingsStream<'r, E: Executor + ?Sized> {
    exec: &'r E,
    op: Box<dyn crate::exec::op::Op + 'r>,
    /// Rows pulled by `next_chunk` but not yet handed out by the row-wise
    /// `Iterator` view. `next_chunk` drains this first, so mixing the two
    /// access styles never loses or reorders rows.
    buf: std::vec::IntoIter<Bindings>,
}

impl<'r, E: Executor + ?Sized> BindingsStream<'r, E> {
    /// Decoded rows of the next operator chunk (≤ `batch_rows()` rows), or
    /// `None` at end of stream. Chunks are never empty (`Op` invariant:
    /// operators never yield `Some(empty)` mid-stream).
    pub fn next_chunk(&mut self) -> Result<Option<Vec<Bindings>>> {
        let buffered: Vec<Bindings> = self.buf.by_ref().collect();
        if !buffered.is_empty() {
            return Ok(Some(buffered));
        }
        match self.op.next()? {
            Some(batch) => Ok(Some(batch.to_bindings(|id| self.exec.decode_term(id))?)),
            None => Ok(None),
        }
    }
}

/// Row-at-a-time convenience view (ASK, library callers).
impl<'r, E: Executor + ?Sized> Iterator for BindingsStream<'r, E> {
    type Item = Result<Bindings>;
    fn next(&mut self) -> Option<Result<Bindings>> {
        loop {
            if let Some(b) = self.buf.next() {
                return Some(Ok(b));
            }
            match self.next_chunk() {
                Ok(Some(rows)) => self.buf = rows.into_iter(),
                Ok(None) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}
```

Note on lifetimes: `build<'r>(&'r self, plan: &PhysicalPlan) -> Result<Box<dyn Op + 'r>>` (`src/exec/op/mod.rs:79`) ties the boxed operators to `&self` only — the plan argument has an unrelated anonymous lifetime, so the compiler guarantees no operator borrows the rewritten local `plan`. Returning `BindingsStream` after `plan` drops is sound by construction.

- [ ] **Step 4: Run the new tests and the runtime suites**

Run: `cargo nextest run -p horndb-sparql run_stream_chunks_match_run run_stream_iterator_matches_run`
Expected: PASS (2 tests).

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — `run`'s signature and semantics are unchanged, so the full suite (including `slot_differential` proptests and `pushdown` invariance tests) stays green.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/exec/runtime.rs crates/sparql/src/exec/op/chunk_tests.rs
git commit -m 'feat(sparql): add Runtime::run_stream chunked streaming handle (#128)'
```

---

### Task 2: ASK early-exit via `run_stream`

Today the ASK arm calls `run(&plan)` — which drains the **entire** result — then checks `.next().is_some()`. Switching to `run_stream` restores genuine early exit: only the first operator chunk is pulled and decoded.

**Files:**
- Modify: `crates/sparql/src/api.rs:98-107` (the `ParsedQuery::Ask` arm)
- Test: `crates/sparql/tests/exec_ask.rs` (append at end)

- [ ] **Step 1: Write the failing test**

The observable: a backend with 5000 id-rows where decoding row ≥ 4096 fails. `batch_rows()` is fixed at 4096 outside `cfg(test)` builds, so chunk 1 decodes cleanly and chunk 2 errors. Before this change ASK drains everything → `Err`; after, it stops at chunk 1 → `Ok(true)`.

Append to `crates/sparql/tests/exec_ask.rs`:

```rust
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
                    .map(|i| Row(vec![Slot::Id(TermId(i)), Slot::Id(TermId(i)), Slot::Id(TermId(i))]))
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql ask_answers_from_first_chunk_without_draining`
Expected: FAIL — `execute_query` returns `Err(Executor("decode of 4096 past the first chunk"))` because `run` drains all 5000 rows.

- [ ] **Step 3: Switch the ASK arm to `run_stream`**

In `crates/sparql/src/api.rs`, replace the `ParsedQuery::Ask` arm's exec block (currently lines 101-105):

```rust
        ParsedQuery::Ask { inner } => {
            let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
            let plan = timed(Stage::Plan, || planner::plan(&alg))?;
            let any = timed(Stage::Exec, || {
                // Early exit: only the first operator chunk is pulled and
                // decoded — `run` would drain the whole result set.
                let rt = Runtime::new(exec);
                let mut stream = rt.run_stream(&plan)?;
                Ok(stream.next_chunk()?.is_some())
            })?;
            Ok(QueryAnswer::Boolean(any))
        }
```

- [ ] **Step 4: Run the test and the ASK/api suites**

Run: `cargo nextest run -p horndb-sparql ask_answers_from_first_chunk_without_draining`
Expected: PASS.

Run: `cargo nextest run -p horndb-sparql -E 'binary(exec_ask) or binary(api_end_to_end)'`
Expected: PASS — ASK semantics unchanged for real backends.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/api.rs crates/sparql/tests/exec_ask.rs
git commit -m 'perf(sparql): ASK answers from the first result chunk (#128)'
```

---

### Task 3: Incremental SELECT serializers (all four formats)

Each serializer becomes header/chunk/footer; the existing one-shot `write_select_*` functions are reimplemented as wrappers so there is exactly one serialization code path and all existing tests (including the sparesults XML round-trip) gate the refactor.

**Files:**
- Modify: `crates/sparql/src/results/mod.rs` (trait + factory)
- Modify: `crates/sparql/src/results/json.rs`, `xml.rs`, `csv.rs`, `tsv.rs`
- Test: `crates/sparql/tests/results_streaming.rs` (new)

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/tests/results_streaming.rs`:

```rust
//! Chunking invariance for the incremental SELECT serializers (#128 HTTP
//! streaming): header + chunks + footer must byte-equal the one-shot
//! writers for every chunk split, in all four formats.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::Bindings;
use horndb_sparql::results::csv::write_select_csv;
use horndb_sparql::results::json::write_select_json;
use horndb_sparql::results::tsv::write_select_tsv;
use horndb_sparql::results::xml::write_select_xml;
use horndb_sparql::results::{select_serializer, ResultFormat};

/// 5 rows over (?s, ?o); odd rows leave ?o unbound; values exercise CSV
/// quoting (comma), XML escaping (`<`), and language-tagged literals.
fn fixture() -> (Vec<String>, Vec<Bindings>) {
    let vars = vec!["s".to_string(), "o".to_string()];
    let rows = (0..5)
        .map(|i| {
            let mut b = Bindings::new();
            b.set("s", Term::Iri(format!("http://ex/s{i}")));
            if i % 2 == 0 {
                b.set("o", Term::Literal(format!("\"v{i},<x>\"@en")));
            }
            b
        })
        .collect();
    (vars, rows)
}

fn incremental(fmt: ResultFormat, vars: &[String], rows: &[Bindings], chunk: usize) -> String {
    let mut ser = select_serializer(fmt);
    let mut out = ser.header(vars);
    for c in rows.chunks(chunk) {
        out.push_str(&ser.chunk(vars, c));
    }
    out.push_str(&ser.footer());
    out
}

#[test]
fn incremental_equals_one_shot_for_every_format_and_chunking() {
    let (vars, rows) = fixture();
    let cases: [(ResultFormat, fn(&[String], &[Bindings]) -> String); 4] = [
        (ResultFormat::Json, write_select_json),
        (ResultFormat::Xml, write_select_xml),
        (ResultFormat::Csv, write_select_csv),
        (ResultFormat::Tsv, write_select_tsv),
    ];
    for (fmt, one_shot) in cases {
        let expected = one_shot(&vars, &rows);
        for chunk in [1, 2, 3, 5] {
            assert_eq!(
                incremental(fmt, &vars, &rows, chunk),
                expected,
                "{fmt:?} diverges at chunk size {chunk}"
            );
        }
    }
}

#[test]
fn zero_row_stream_is_well_formed() {
    let (vars, _) = fixture();
    let cases: [(ResultFormat, fn(&[String], &[Bindings]) -> String); 4] = [
        (ResultFormat::Json, write_select_json),
        (ResultFormat::Xml, write_select_xml),
        (ResultFormat::Csv, write_select_csv),
        (ResultFormat::Tsv, write_select_tsv),
    ];
    for (fmt, one_shot) in cases {
        let mut ser = select_serializer(fmt);
        let mut out = ser.header(&vars);
        out.push_str(&ser.footer());
        assert_eq!(out, one_shot(&vars, &[]), "{fmt:?} empty result diverges");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'binary(results_streaming)'`
Expected: build FAILURE — `unresolved import horndb_sparql::results::select_serializer`.

- [ ] **Step 3: Add the trait and factory to `results/mod.rs`**

Append to `crates/sparql/src/results/mod.rs`:

```rust
use crate::exec::Bindings;

/// Incremental SELECT serializer: `header` → zero or more `chunk`s →
/// `footer`. Invariant (gated by `tests/results_streaming.rs`):
/// concatenating the pieces over ANY chunking of the same rows yields
/// exactly the one-shot `write_select_*` document — those functions are
/// wrappers over this trait, so there is a single serialization path.
pub trait SelectSerializer {
    fn header(&mut self, vars: &[String]) -> String;
    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String;
    fn footer(&mut self) -> String;
}

/// Construct the incremental serializer for a wire format.
pub fn select_serializer(fmt: ResultFormat) -> Box<dyn SelectSerializer + Send> {
    match fmt {
        ResultFormat::Json => Box::new(json::JsonSelectSerializer::default()),
        ResultFormat::Xml => Box::new(xml::XmlSelectSerializer),
        ResultFormat::Csv => Box::new(csv::CsvSelectSerializer),
        ResultFormat::Tsv => Box::new(tsv::TsvSelectSerializer),
    }
}
```

- [ ] **Step 4: JSON — incremental impl, one-shot wrapper**

In `crates/sparql/src/results/json.rs`, replace `write_select_json` (lines 9-28) with:

```rust
use crate::results::SelectSerializer;

/// Incremental SPARQL-JSON SELECT serializer. The only cross-chunk state is
/// the comma placement between binding objects.
#[derive(Default)]
pub struct JsonSelectSerializer {
    any_rows: bool,
}

impl SelectSerializer for JsonSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        format!(
            "{{\"head\":{{\"vars\":{}}},\"results\":{{\"bindings\":[",
            serde_json::to_string(vars).expect("a Vec<String> always serializes")
        )
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            if self.any_rows {
                out.push(',');
            }
            self.any_rows = true;
            let mut obj = Map::new();
            for v in vars {
                if let Some(t) = row.get(v) {
                    obj.insert(v.clone(), term_to_json(t));
                }
            }
            out.push_str(&Value::Object(obj).to_string());
        }
        out
    }

    fn footer(&mut self) -> String {
        "]}}".to_string()
    }
}

pub fn write_select_json(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = JsonSelectSerializer::default();
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}
```

(`json!` import may become unused in this file except in `write_ask_json`/`parse_literal_to_json` — leave those functions untouched; adjust `use serde_json::{json, Map, Value};` only if the compiler warns.)

- [ ] **Step 5: XML — incremental impl, one-shot wrapper**

In `crates/sparql/src/results/xml.rs`, replace `write_select_xml` (lines 15-43) with:

```rust
use crate::results::SelectSerializer;

/// Incremental SPARQL-XML SELECT serializer. Stateless: `<result>` blocks
/// are self-contained, so chunks need no cross-chunk bookkeeping.
pub struct XmlSelectSerializer;

impl SelectSerializer for XmlSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        out.push_str(r#"<?xml version="1.0"?>"#);
        out.push_str("\n<sparql xmlns=\"http://www.w3.org/2005/sparql-results#\">\n");
        out.push_str("  <head>\n");
        for v in vars {
            out.push_str(&format!(
                "    <variable name=\"{}\"/>\n",
                xml_attr_escape(v)
            ));
        }
        out.push_str("  </head>\n");
        out.push_str("  <results>\n");
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            out.push_str("    <result>\n");
            for v in vars {
                if let Some(t) = row.get(v) {
                    out.push_str(&format!("      <binding name=\"{}\">", xml_attr_escape(v)));
                    out.push_str(&term_to_xml(t));
                    out.push_str("</binding>\n");
                }
            }
            out.push_str("    </result>\n");
        }
        out
    }

    fn footer(&mut self) -> String {
        "  </results>\n</sparql>\n".to_string()
    }
}

/// Serialise a SELECT result set as SPARQL Results XML.
pub fn write_select_xml(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = XmlSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}
```

- [ ] **Step 6: CSV and TSV — incremental impls, one-shot wrappers**

In `crates/sparql/src/results/csv.rs`, replace `write_select_csv` (lines 6-22) with:

```rust
use crate::results::SelectSerializer;

/// Incremental SPARQL-CSV SELECT serializer. Stateless: lines are
/// self-contained.
pub struct CsvSelectSerializer;

impl SelectSerializer for CsvSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        out.push_str(&vars.join(","));
        out.push_str("\r\n");
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            let cells: Vec<String> = vars
                .iter()
                .map(|v| match row.get(v) {
                    None => String::new(),
                    Some(t) => csv_escape(&term_to_lex(t)),
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push_str("\r\n");
        }
        out
    }

    fn footer(&mut self) -> String {
        String::new()
    }
}

pub fn write_select_csv(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = CsvSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}
```

In `crates/sparql/src/results/tsv.rs`, replace `write_select_tsv` (lines 6-36) with:

```rust
use crate::results::SelectSerializer;

/// Incremental SPARQL-TSV SELECT serializer. Stateless: lines are
/// self-contained.
pub struct TsvSelectSerializer;

impl SelectSerializer for TsvSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        let header: Vec<String> = vars.iter().map(|v| format!("?{v}")).collect();
        out.push_str(&header.join("\t"));
        out.push('\n');
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            let cells: Vec<String> = vars
                .iter()
                .map(|v| match row.get(v) {
                    None => String::new(),
                    Some(Term::Iri(s)) => format!("<{s}>"),
                    Some(Term::BlankNode(s)) => {
                        if s.starts_with("_:") {
                            s.clone()
                        } else {
                            format!("_:{s}")
                        }
                    }
                    Some(Term::Literal(s)) => s.clone(),
                    Some(Term::Var(v)) => format!("?{}", v.name()),
                    // RDF 1.2 triple-term solution-mapping values: TSV has no
                    // canonical encoding; emit empty (the SPARQL 1.1 "unbound"
                    // shape) until SPEC-07 RDF 1.2 follow-up.
                    Some(Term::Triple(_)) => String::new(),
                })
                .collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
        out
    }

    fn footer(&mut self) -> String {
        String::new()
    }
}

pub fn write_select_tsv(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = TsvSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}
```

- [ ] **Step 7: Run the new tests plus every existing results consumer**

Run: `cargo nextest run -p horndb-sparql -E 'binary(results_streaming) or binary(results_json) or binary(server_http)' --features server`
Expected: PASS — including `select_xml_is_well_formed_and_roundtrips` (in-crate `results/xml.rs` tests run under the default profile too; run `cargo nextest run -p horndb-sparql` if in doubt).

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/results/ crates/sparql/tests/results_streaming.rs
git commit -m 'refactor(sparql): incremental header/chunk/footer SELECT serializers (#128)'
```

---

### Task 4: `api::plan_select` — planning-only SELECT entry point

The handler needs parse→translate→plan **without** execution (execution moves to a blocking thread that owns the store guard). Planning needs no executor: `planner::plan` takes only the algebra, and the pushdown rewrite/`count_bgp` consultation happen inside `run_stream`/`build`.

**Files:**
- Modify: `crates/sparql/src/api.rs` (new function after `execute_query_with`)
- Test: `crates/sparql/tests/api_end_to_end.rs` (append at end)

- [ ] **Step 1: Write the failing test**

Append to `crates/sparql/tests/api_end_to_end.rs`:

```rust
#[test]
fn plan_select_routes_only_plain_select() {
    use horndb_sparql::api::plan_select;
    use horndb_sparql::SparqlConfig;

    let cfg = SparqlConfig::default();
    let (vars, _plan) = plan_select("SELECT ?s ?o WHERE { ?s ?p ?o }", &cfg)
        .unwrap()
        .expect("a plain SELECT plans for streaming");
    assert_eq!(vars, vec!["s".to_string(), "o".to_string()]);

    for q in [
        "ASK { ?s ?p ?o }",
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        "DESCRIBE <http://ex/a>",
        "EXPLAIN SELECT ?s WHERE { ?s ?p ?o }",
    ] {
        assert!(
            plan_select(q, &cfg).unwrap().is_none(),
            "{q} must fall back to execute_query"
        );
    }

    assert!(plan_select("SELECT ?s WHERE { ?s", &cfg).is_err());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql plan_select_routes_only_plain_select`
Expected: build FAILURE — `unresolved import horndb_sparql::api::plan_select`.

- [ ] **Step 3: Implement `plan_select`**

In `crates/sparql/src/api.rs`, add after `execute_query_with` (after line 157):

```rust
/// Parse → translate → plan a query for streaming execution, without
/// running it. Returns `Some((projected_vars, plan))` for a plain SELECT;
/// `None` for every other form (ASK / CONSTRUCT / DESCRIBE / EXPLAIN),
/// which the caller answers via [`execute_query`]. Records the same
/// Parse/Translate/Plan stage metrics as `execute_query`;
/// `query_total{kind=select}` is bumped only on the `Some` path so the
/// fallback keeps per-kind counts exact (a non-SELECT query costs one
/// extra `parse` stage observation from the routing double-parse — noted
/// in `docs/metrics.md`).
pub fn plan_select(
    query: &str,
    cfg: &SparqlConfig,
) -> Result<Option<(Vec<String>, crate::plan::PhysicalPlan)>> {
    let parsed = timed(Stage::Parse, || parse_query(query))?;
    let ParsedQuery::Select { inner } = parsed else {
        return Ok(None);
    };
    horndb_metrics::metrics()
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel {
            kind: QueryKind::Select,
        })
        .inc();
    let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
    let vars = projected_vars(&alg);
    let plan = timed(Stage::Plan, || planner::plan(&alg))?;
    Ok(Some((vars, plan)))
}
```

(All names are already imported at the top of `api.rs`: `parse_query`, `ParsedQuery`, `translate_query_with`, `planner`, `QueryKind`, `QueryKindLabel`, `Stage`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p horndb-sparql plan_select_routes_only_plain_select`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/api.rs crates/sparql/tests/api_end_to_end.rs
git commit -m 'feat(sparql): plan_select planning-only entry point for streaming (#128)'
```

---

### Task 5: `ChannelBody` — chunked response body over an mpsc channel

A hand-rolled `http_body::Body` (same pattern as the existing `server/counting_body.rs`) — **no** `tokio-stream`/`http-body-util` dependency. Yields the pre-buffered first chunk, then whatever the blocking serializer sends. An `Err` item makes hyper abort the response without the chunked-encoding terminator — the mid-stream error contract.

**Files:**
- Create: `crates/sparql/src/server/stream_body.rs`
- Modify: `crates/sparql/src/server/mod.rs:7` (module declaration)

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/src/server/stream_body.rs` with the tests first (the type doesn't exist yet, so this fails to build):

```rust
//! Streaming response body fed by a bounded channel from the blocking
//! serializer thread (see `server/query.rs::stream_select`). Design:
//! `docs/specs/SPEC-22-http-streaming-results.md`.

#[cfg(test)]
mod tests {
    use super::ChannelBody;
    use crate::error::SparqlError;
    use bytes::Bytes;
    use http_body::Body as _;
    use std::pin::Pin;

    #[tokio::test]
    async fn yields_first_then_channel_frames_then_ends() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut body = ChannelBody::new(Bytes::from_static(b"head+chunk1"), rx);
        tx.send(Ok(Bytes::from_static(b"chunk2"))).await.unwrap();
        tx.send(Ok(Bytes::from_static(b"footer"))).await.unwrap();
        drop(tx);

        let mut got: Vec<Bytes> = Vec::new();
        while let Some(frame) =
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
        {
            got.push(frame.expect("clean stream").into_data().expect("data frame"));
        }
        assert_eq!(
            got,
            vec![
                Bytes::from_static(b"head+chunk1"),
                Bytes::from_static(b"chunk2"),
                Bytes::from_static(b"footer"),
            ]
        );
    }

    #[tokio::test]
    async fn err_item_surfaces_as_body_error() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut body = ChannelBody::new(Bytes::from_static(b"head"), rx);
        tx.send(Err(SparqlError::Executor("mid-stream".into())))
            .await
            .unwrap();
        drop(tx);

        let first = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap();
        assert!(first.is_ok(), "pre-buffered first chunk is clean");
        let second = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap();
        assert!(second.is_err(), "the Err item must abort the body");
    }
}
```

And declare the module in `crates/sparql/src/server/mod.rs`, next to the existing `mod counting_body;` (line 7):

```rust
mod stream_body;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql --features server -E 'test(yields_first_then_channel_frames_then_ends) or test(err_item_surfaces_as_body_error)'`
Expected: build FAILURE — `cannot find type ChannelBody`.

- [ ] **Step 3: Implement `ChannelBody`**

Add above the `#[cfg(test)]` module in `crates/sparql/src/server/stream_body.rs`:

```rust
use crate::error::SparqlError;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Receiver;

/// `http_body::Body` over the pre-buffered first chunk (header + first
/// result chunk, produced before the 200 was committed) followed by
/// whatever the blocking serializer sends. An `Err` item aborts the
/// response mid-body: hyper drops the connection without the
/// chunked-encoding terminator (HTTP/2: RST_STREAM), so clients detect the
/// truncation at the protocol level.
///
/// Dropping this body drops the receiver, which makes the serializer's
/// `blocking_send` fail — the blocking task returns early and releases the
/// store read lock. That is the client-disconnect cancellation path.
pub(crate) struct ChannelBody {
    first: Option<Bytes>,
    rx: Receiver<Result<Bytes, SparqlError>>,
}

impl ChannelBody {
    pub(crate) fn new(first: Bytes, rx: Receiver<Result<Bytes, SparqlError>>) -> Self {
        Self {
            first: Some(first),
            rx,
        }
    }
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = SparqlError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, SparqlError>>> {
        let this = self.get_mut();
        if let Some(b) = this.first.take() {
            return Poll::Ready(Some(Ok(Frame::data(b))));
        }
        this.rx
            .poll_recv(cx)
            .map(|opt| opt.map(|r| r.map(Frame::data)))
    }

    fn size_hint(&self) -> SizeHint {
        // Length unknown until the stream ends — forces chunked encoding.
        SizeHint::default()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql --features server -E 'test(yields_first_then_channel_frames_then_ends) or test(err_item_surfaces_as_body_error)'`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/server/stream_body.rs crates/sparql/src/server/mod.rs
git commit -m 'feat(sparql): ChannelBody chunked HTTP body over a bounded mpsc (#128)'
```

---

### Task 6: Streaming SELECT path in the `/query` handler

The handler routes: `plan_select` → `Some` streams via `spawn_blocking` + channel; `None` falls through to today's materialized path, verbatim. The read guard and the operator tree stay on the blocking thread (`!Send` — see the design spec); only `Bytes` crosses. The first chunk is pre-buffered so early exec errors are still a clean 400.

**Files:**
- Modify: `crates/sparql/src/server/query.rs` (the `run` function, lines 100-169, plus imports)
- Modify: `docs/metrics.md:80-81` (semantics note — same commit, per the root `CLAUDE.md` metrics-sync rule)
- Test: `crates/sparql/tests/server_http.rs` (append at end)

- [ ] **Step 1: Write the failing test**

Append to `crates/sparql/tests/server_http.rs`:

```rust
/// 5000 rows is above the fixed release batch_rows() of 4096, so a streamed
/// body must arrive in >= 2 data frames. One frame == the old materialized
/// path (this is the memory-win mechanism proof: multiple frames means the
/// full serialized document never existed in one buffer).
#[tokio::test]
async fn large_select_streams_in_multiple_chunks() {
    use http_body::Body as _;

    let mut s = MemStore::default();
    for i in 0..5000 {
        s.insert_triple(
            iri(&format!("http://ex/s{i}")),
            iri("http://ex/p"),
            iri(&format!("http://ex/o{i}")),
        );
    }
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
    };
    let app = build_router(state);

    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fs%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["content-type"],
        "application/sparql-results+json"
    );

    let mut body = resp.into_body();
    let mut frames = 0usize;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.expect("clean stream");
        if let Ok(data) = frame.into_data() {
            frames += 1;
            buf.extend_from_slice(&data);
        }
    }
    assert!(frames >= 2, "expected a chunked body, got {frames} frame(s)");
    let v: serde_json::Value = serde_json::from_slice(&buf).expect("frames concatenate to valid JSON");
    assert_eq!(v["results"]["bindings"].as_array().unwrap().len(), 5000);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql --features server large_select_streams_in_multiple_chunks`
Expected: FAIL on `frames >= 2` — the materialized path returns the whole body as one frame.

- [ ] **Step 3: Rewrite `run` in `server/query.rs` and add the streaming path**

Replace the imports block at the top of `crates/sparql/src/server/query.rs` (lines 6-16) with:

```rust
use super::stream_body::ChannelBody;
use super::AppState;
use crate::api::{execute_query, plan_select, QueryAnswer};
use crate::error::SparqlError;
use crate::exec::runtime::Runtime;
use crate::exec::FullBackend;
use crate::plan::PhysicalPlan;
use crate::results::{
    csv::write_select_csv, json::write_ask_json, json::write_select_json, select_serializer,
    tsv::write_select_tsv, xml::write_ask_xml, xml::write_select_xml, ResultFormat,
};
use crate::SparqlConfig;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use horndb_metrics::labels::{Stage, StageLabel};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
```

Replace the existing `async fn run` (lines 100-169) with the router + the streaming function + the renamed materialized fallback. The `run_materialized` body is today's `run` body unchanged except that `fmt` is now a parameter:

```rust
async fn run<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    q: &str,
    headers: &HeaderMap,
) -> axum::response::Response {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let fmt = ResultFormat::from_accept(accept);

    // Plain SELECTs stream; everything else (ASK / CONSTRUCT / DESCRIBE /
    // EXPLAIN) keeps the materialized path — their results are small.
    // Planning needs no store access, so it runs here on the async thread.
    match plan_select(q, &SparqlConfig::default()) {
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Ok(Some((vars, plan))) => stream_select(state, vars, plan, fmt).await,
        Ok(None) => run_materialized(state, q, fmt).await,
    }
}

/// Serialized chunks buffered between the blocking serializer thread and
/// the async body. Bounded: a slow client exerts backpressure on the
/// executor instead of buffering the whole result.
const STREAM_CHANNEL_CHUNKS: usize = 8;

/// Mirror `api::timed(Stage::Exec, …)` for the streaming path: observe the
/// stage duration (here: time to first chunk — the full drain is visible in
/// `request_duration_seconds`) and bump `query_errors` on error.
fn record_exec(start: Instant, err: bool) {
    let m = horndb_metrics::metrics();
    let label = StageLabel { stage: Stage::Exec };
    m.sparql
        .stage_duration_seconds
        .get_or_create(&label)
        .observe(start.elapsed().as_secs_f64());
    if err {
        m.sparql.query_errors.get_or_create(&label).inc();
    }
}

/// Bump `query_errors{stage=exec}` for an error after the exec stage was
/// already observed (mid-stream failure).
fn bump_exec_error() {
    horndb_metrics::metrics()
        .sparql
        .query_errors
        .get_or_create(&StageLabel { stage: Stage::Exec })
        .inc();
}

/// Execute + decode + serialize a SELECT on a blocking thread, streaming
/// serialized `Bytes` chunks to the response body over a bounded channel.
///
/// Everything store-touching stays on the one blocking thread: the
/// `RwLockReadGuard` and the operator tree (`Box<dyn Op>`, which borrows
/// through the guard) are `!Send`. The first chunk is decoded BEFORE any
/// bytes are emitted, so build/scan/first-decode errors return a clean 400;
/// after that, an error aborts the chunked body (see `ChannelBody`).
///
/// Trade-off (accepted, see the 2026-07-06 design spec): the read lock is
/// held until the client drains the response, so a slow download blocks
/// writers (not readers). SPEC-02 MVCC removes this; the bounded channel
/// plus the send-failure-on-disconnect path bound the damage a dead client
/// can do.
async fn stream_select<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    vars: Vec<String>,
    plan: PhysicalPlan,
    fmt: ResultFormat,
) -> axum::response::Response {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, SparqlError>>(STREAM_CHANNEL_CHUNKS);
    let store = Arc::clone(&state.store);

    tokio::task::spawn_blocking(move || {
        let store = store.read().unwrap();
        let rt = Runtime::new(&*store);
        let mut ser = select_serializer(fmt);
        let start = Instant::now();

        let mut stream = match rt.run_stream(&plan) {
            Ok(s) => s,
            Err(e) => {
                record_exec(start, true);
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        // Pre-buffer chunk 1 so its errors surface before headers commit.
        let first_rows = match stream.next_chunk() {
            Ok(r) => r,
            Err(e) => {
                record_exec(start, true);
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        record_exec(start, false);

        let mut head = ser.header(&vars);
        match first_rows {
            Some(rows) => head.push_str(&ser.chunk(&vars, &rows)),
            None => {
                // Empty result: one frame carrying the whole document.
                head.push_str(&ser.footer());
                let _ = tx.blocking_send(Ok(Bytes::from(head)));
                return;
            }
        }
        if tx.blocking_send(Ok(Bytes::from(head))).is_err() {
            return; // client disconnected — release the read lock
        }
        loop {
            match stream.next_chunk() {
                Ok(Some(rows)) => {
                    let bytes = Bytes::from(ser.chunk(&vars, &rows));
                    if tx.blocking_send(Ok(bytes)).is_err() {
                        return; // client disconnected
                    }
                }
                Ok(None) => {
                    let _ = tx.blocking_send(Ok(Bytes::from(ser.footer())));
                    return;
                }
                Err(e) => {
                    // Headers are committed: abort the body (see ChannelBody).
                    bump_exec_error();
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });

    match rx.recv().await {
        Some(Ok(first)) => {
            let body = axum::body::Body::new(ChannelBody::new(first, rx));
            (
                StatusCode::OK,
                [("content-type", fmt.content_type())],
                body,
            )
                .into_response()
        }
        // Errors before any byte was emitted are still a clean 400 —
        // parity with the materialized path's error handling.
        Some(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "result stream ended before producing output".to_string(),
        )
            .into_response(),
    }
}

/// Materialized path for non-SELECT forms (and the pre-streaming behavior):
/// execute fully, then serialize in one shot. Body identical to the old
/// `run` except `fmt` is passed in.
async fn run_materialized<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    q: &str,
    fmt: ResultFormat,
) -> axum::response::Response {
    // Scope the read guard to the execution only; results are
    // materialised into `ans`, so serialization below holds no lock and
    // never blocks a concurrent writer.
    let ans = {
        let store = state.store.read().unwrap();
        match execute_query(q, &*store) {
            Ok(a) => a,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
        }
    };

    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            // Unreachable for plain SELECTs (they take stream_select), but
            // kept for defense in depth — behavior is identical.
            let body = match fmt {
                ResultFormat::Json => write_select_json(&vars, &rows),
                ResultFormat::Xml => write_select_xml(&vars, &rows),
                ResultFormat::Csv => write_select_csv(&vars, &rows),
                ResultFormat::Tsv => write_select_tsv(&vars, &rows),
            };
            (StatusCode::OK, [("content-type", fmt.content_type())], body).into_response()
        }
        QueryAnswer::Boolean(b) => {
            // CSV/TSV have no boolean serialisation; fall back to XML
            // (the protocol default for ASK in many clients) for those.
            let (ctype, body) = match fmt {
                ResultFormat::Json => (ResultFormat::Json.content_type(), write_ask_json(b)),
                _ => (ResultFormat::Xml.content_type(), write_ask_xml(b)),
            };
            (StatusCode::OK, [("content-type", ctype)], body).into_response()
        }
        QueryAnswer::Triples(triples) => {
            // Stage 1: serialise CONSTRUCT as N-Triples.
            let mut s = String::new();
            for (sub, p, o) in triples {
                s.push_str(&format!("<{sub}> <{p}> <{o}> .\n"));
            }
            (
                StatusCode::OK,
                [("content-type", "application/n-triples")],
                s,
            )
                .into_response()
        }
        QueryAnswer::Explanation { text, json } => {
            // EXPLAIN (SPEC-07 F9): the plan rendering. The format is
            // fixed by the pragma (`EXPLAIN` vs `EXPLAIN JSON`), not the
            // Accept header, since EXPLAIN output is not a SPARQL results
            // document.
            let ctype = if json {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            };
            (StatusCode::OK, [("content-type", ctype)], text).into_response()
        }
    }
}
```

- [ ] **Step 4: Update `docs/metrics.md` (same commit — metrics-sync rule)**

In `docs/metrics.md`, line 81, change the `horndb_sparql_stage_duration_seconds` row's meaning cell from `per-stage pipeline latency` to:

```
per-stage pipeline latency; for HTTP-streamed SELECTs, `exec` measures plan→first-result-chunk (the full drain is visible in `horndb_sparql_request_duration_seconds`), and non-SELECT `/query` requests record one extra `parse` observation from streaming-path routing
```

Line 80 (`horndb_sparql_query_errors_total`), change the meaning cell from `pipeline errors by stage` to:

```
pipeline errors by stage; `exec` includes mid-stream errors of HTTP-streamed SELECTs (which abort the response body rather than producing a 4xx/5xx)
```

- [ ] **Step 5: Run the new test and the full server suite**

Run: `cargo nextest run -p horndb-sparql --features server large_select_streams_in_multiple_chunks`
Expected: PASS.

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS — all existing `server_http.rs`, `metrics_endpoint.rs`, `metrics_pipeline.rs`, `latency_smoke.rs` tests green (small SELECT results now arrive via the streaming path as a single frame plus footer; `axum::body::to_bytes` in the existing tests concatenates frames, so their assertions are unchanged).

Note: if `metrics_pipeline.rs` asserts exact per-stage sample counts for SELECT-over-HTTP, the streaming path's stage accounting (`record_exec`) keeps one `exec` observation per query — parity. If a count assertion fails, read that test and adjust the expectation only if it was counting the *ask-vs-select* mix, not the totals.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/server/query.rs docs/metrics.md
git commit -m 'feat(sparql): stream SELECT results to HTTP chunk-by-chunk (#128)'
```

---

### Task 7: Error-semantics integration tests (pre-header 400, mid-stream abort)

These pin the contract from the design spec: errors before the first chunk are a clean 400; errors after headers abort the chunked body and are observable as a body read error.

**Files:**
- Test: `crates/sparql/tests/server_http.rs` (append at end)

- [ ] **Step 1: Write the tests**

Append to `crates/sparql/tests/server_http.rs`:

```rust
mod streaming_error_semantics {
    use super::*;
    use horndb_sparql::algebra::{TriplePattern, Var};
    use horndb_sparql::exec::{Batch, Bindings, Executor, Row, Slot};
    use horndb_sparql::SparqlError;
    use horndb_storage::TermId;

    /// Backend whose scan fails immediately: the error lands before the
    /// first chunk, so the response must be a clean 400.
    struct FailingScan;

    impl Executor for FailingScan {
        fn scan_bgp(
            &self,
            _patterns: &[TriplePattern],
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            Err(SparqlError::Executor("scan exploded".into()))
        }
    }
    impl horndb_sparql::exec::Store for FailingScan {
        fn insert_triple(&mut self, _s: Term, _p: Term, _o: Term) {}
        fn delete_triple(&mut self, _s: &Term, _p: &Term, _o: &Term) {}
        fn clear_all(&mut self) {}
    }

    #[tokio::test]
    async fn exec_error_before_first_chunk_returns_400() {
        let state = AppState {
            store: Arc::new(RwLock::new(FailingScan)),
        };
        let app = build_router(state);
        let req = Request::builder()
            .uri("/query?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 5000 id-rows; decoding any id >= 4096 fails. Chunk 1 (4096 rows)
    /// serializes and commits the 200; the failure lands in chunk 2, so the
    /// body must abort mid-stream (protocol-level truncation), NOT morph
    /// into an error status.
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
                Err(SparqlError::Executor("decode failed mid-stream".into()))
            }
        }
    }
    impl horndb_sparql::exec::Store for DecodeFailsLate {
        fn insert_triple(&mut self, _s: Term, _p: Term, _o: Term) {}
        fn delete_triple(&mut self, _s: &Term, _p: &Term, _o: &Term) {}
        fn clear_all(&mut self) {}
    }

    #[tokio::test]
    async fn exec_error_mid_stream_aborts_body_after_200() {
        use http_body::Body as _;

        let state = AppState {
            store: Arc::new(RwLock::new(DecodeFailsLate)),
        };
        let app = build_router(state);
        // SELECT all three vars so column pruning keeps every column.
        let req = Request::builder()
            .uri("/query?query=SELECT%20%3Fs%20%3Fp%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
            .header("accept", "text/csv")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "headers are already committed when the error hits"
        );

        let mut body = resp.into_body();
        let mut data_frames = 0usize;
        let mut saw_error = false;
        while let Some(frame) =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            match frame {
                Ok(f) => {
                    if f.into_data().is_ok() {
                        data_frames += 1;
                    }
                }
                Err(_) => {
                    saw_error = true;
                    break;
                }
            }
        }
        assert!(data_frames >= 1, "chunk 1 was delivered before the error");
        assert!(saw_error, "the body must surface the mid-stream error");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo nextest run -p horndb-sparql --features server -E 'test(exec_error_before_first_chunk_returns_400) or test(exec_error_mid_stream_aborts_body_after_200)'`
Expected: PASS — Task 6's implementation already provides both behaviors; these tests pin the contract. If either fails, that is a real Task-6 bug: debug the handler (the 400 test failing means the first-chunk pre-buffer isn't happening before `rx.recv()` resolves; the abort test failing usually means an `Err` item is being swallowed instead of forwarded to the channel).

- [ ] **Step 3: Run the whole server suite**

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add crates/sparql/tests/server_http.rs
git commit -m 'test(sparql): pin pre-header 400 and mid-stream abort semantics (#128)'
```

---

### Task 8: Final gates, integration notes, hornbench measurements

**Files:**
- Modify: `crates/sparql/INTEGRATION-NOTES.md` (append a short section)

- [ ] **Step 1: Full test gates**

Run each and confirm green:

```bash
cargo fmt --all -- --check
cargo nextest run -p horndb-sparql
cargo nextest run -p horndb-sparql --features server
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS / no warnings. (The clippy run pulls in the harness's `oxrocksdb-sys` on first invocation after a clean checkout — several minutes; subsequent runs reuse the cache.)

- [ ] **Step 2: Record the design decisions in `INTEGRATION-NOTES.md`**

Append to `crates/sparql/INTEGRATION-NOTES.md`:

```markdown
## HTTP streaming results (#128, 2026-07-06)

- `Runtime::run_stream` returns a `BindingsStream` (chunked decode at the
  boundary); `run` collects it — signature unchanged. `api::plan_select`
  is the planning-only SELECT entry the streaming handler uses.
- The `/query` handler streams plain SELECTs: exec+decode+serialize run in
  `spawn_blocking` (the store read guard and the `Op` tree are `!Send`),
  serialized `Bytes` cross to a `ChannelBody` over a bounded mpsc.
- Error contract: first chunk is pre-buffered → early errors are HTTP 400;
  mid-stream errors abort the chunked body (no terminator) — clients detect
  truncation at the protocol level. No format can express a trailing error.
- The read lock is now held until the client drains a streamed SELECT
  (writers wait; readers don't). Accepted until SPEC-02 MVCC.
- CONSTRUCT/DESCRIBE streaming deferred (#TODO); UPDATE must stay
  materialized (SPARQL 1.1 §3.1.3 pre-update snapshot semantics).

Full rationale: `docs/specs/SPEC-22-http-streaming-results.md`.
```

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add crates/sparql/INTEGRATION-NOTES.md
git commit -m 'docs(sparql): record HTTP streaming results decisions (#128)'
```

- [ ] **Step 4: Peak-RSS measurement on hornbench (never the laptop)**

```bash
ssh hornbench
cd ~/src/horndb
git fetch && git checkout <this branch>
cargo build --release -p horndb-sparql --features server --bin serve

# ~5M-triple synthetic graph (~300 MB .nt)
seq 0 4999999 | awk '{ printf "<http://ex/s%d> <http://ex/p> <http://ex/o%d> .\n", $1 % 100000, $1 }' > /tmp/streamtest.nt

# Terminal 1 — server under time -v:
/usr/bin/time -v ./target/release/serve --data /tmp/streamtest.nt --bind 127.0.0.1:3840 2> /tmp/serve-rss.log

# Terminal 2 — one full-scan SELECT, drained to /dev/null:
curl -s -H 'accept: application/sparql-results+json' \
  --data-urlencode 'query=SELECT ?s ?p ?o WHERE { ?s ?p ?o }' \
  'http://127.0.0.1:3840/query' -o /dev/null

# Stop the server (Ctrl-C in terminal 1); read "Maximum resident set size"
# from /tmp/serve-rss.log.
```

Repeat the identical procedure on `main` (same data file, same query). Expected: the branch's peak RSS is materially lower — the decoded `Vec<Bindings>` (~5M `BTreeMap<String, Term>` rows) and the fully-serialized body `String` no longer coexist with the id-row scan batch. Record both numbers and the delta.

- [ ] **Step 5: SPB no-regression on hornbench**

SPB aggregation results are small — this increment targets large-SELECT memory, not SPB throughput — but the streaming path adds one `spawn_blocking` dispatch and one channel hop per SELECT, so verify no regression: run the SPB-256 comparison exactly as documented in `docs/benchmarks.md` / the `nightly-benchmarks` skill on hornbench, branch vs same-day main. Expected: `aggregation-qps` and `editorial-qps` within noise of the ~30.8 baseline.

- [ ] **Step 6: Hand off the numbers**

Report the RSS pair and the SPB qps numbers (with commit hashes and host env) back to the integrating session. Do **not** edit `docs/benchmarks.md`, `TASKS.md`, `docs/architecture.md`, or `docs/index.md` from this branch — the integrating session syncs those in the merge commit per the root `CLAUDE.md` doc-sync rule.

---

## Self-review checklist (done at plan-writing time)

- **Spec coverage:** run_stream (Task 1), ASK early-exit (Task 2), incremental serializers all four formats (Task 3), plan_select (Task 4), ChannelBody (Task 5), handler + lock/Send handling + metrics doc (Task 6), error semantics (Task 7), gates + measurements (Task 8). CONSTRUCT/DESCRIBE/UPDATE explicitly out of scope per the design spec.
- **Signatures verified against the codebase 2026-07-06:** `Batch { pub schema, pub rows }` / `Row(pub Vec<Slot>)` / `Slot::Id(TermId)` / `TermId(pub u64)` are all public (`exec/batch.rs`, `storage/src/term.rs`) — the test doubles compile. `Executor::scan_bgp_ids`/`decode_term`/`scan_bgp` defaults per `exec/mod.rs:69-120`. `build`'s lifetime signature (`exec/op/mod.rs:79`) proves operators don't borrow the plan. axum 0.8 / http-body 1 / tokio with `rt-multi-thread`+`macros` per root `Cargo.toml:90-96`.
- **No placeholders:** every code step contains complete code; commit messages are single-quoted (shell-hygiene rule).
