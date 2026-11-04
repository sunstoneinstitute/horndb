# open-ontologies vs HornDB

This page compares [open-ontologies](https://github.com/fabio-rovai/open-ontologies)
(MIT, Rust) with HornDB. It covers where the two overlap and what HornDB could
learn from it or reuse. Read it before scoping proof or provenance work (SPEC-04
F4, SPEC-27), schema-change tooling on top of SPEC-06, or an agent-facing
surface (SPEC-08).

The findings were read from the source tree at commit `9e73d95` (2026-10-26).
File paths are cited inline. The project moves fast, so check the repo again
before you rely on a code-level detail.

## One-line framing

open-ontologies is a tool for **checking changes to an ontology**. It is a
single binary that speaks MCP (Model Context Protocol, the tool interface used
by Claude, Cursor and similar clients) and works on small, in-memory graphs. Its
selling point is evidence: every inference can be exported as a certificate
that a separately proved checker verifies. HornDB is a **database engine**.
It materialises and maintains OWL 2 RL closures at scale. The reasoning cores
overlap. The products do not.

## Shared ground

| Dimension | open-ontologies | HornDB |
|---|---|---|
| Language | Rust (edition 2024), one crate, about 90k lines in `src/` | Rust (edition 2021), nine crates |
| OWL 2 RL forward chaining | `src/reason.rs`: a fixpoint loop over interned triples held in hash sets. Profiles are `rdfs`, `owl-rl` and `owl-rl-ext` | Rules compiled from `rules.toml` into Rust (SPEC-04), semi-naive evaluation, a GraphBLAS closure backend (SPEC-05) |
| Incremental reasoning | `src/reason_incremental.rs`: semi-naive evaluation for additions only. A schema-level addition or a new property characteristic falls back to a full recompute | DBSP-style Z-set deltas (SPEC-06): insertion, delta-based retraction, a change feed |
| Provenance | One `(rule, premises)` record per inferred triple, written out as a certificate. Optional capture of every rule instance applied (the full derivation graph) | `Provenance` side-table and `Engine::proof` (proof trees). Reachable from Rust only |
| SPARQL | Oxigraph | Own frontend, WCOJ (worst-case optimal join) executor |
| Scale evidence | LUBM-100 (13.4M triples) loads in 25 s and materialises in 95 s on an Apple M-series laptop (`benchmark/lubm/RESULTS.md`). They describe the cost as growing faster than linear | LUBM gates in `docs/benchmarks.md`. LUBM-100 has not run yet |

Beyond the overlap, open-ontologies also ships a SHIQ tableaux reasoner
(`src/tableaux.rs`), an EL classifier, SHACL validation, ontology alignment,
data import from CSV, XLSX, Parquet and SQL sources, embeddings, and exporters
to first-order provers (TPTP, SMT-LIB). It also has formal artifacts in Lean 4,
Isabelle, Rocq and Dafny.

## Findings

### F1. Derivation certificates with a proved checker (reuse)

A reasoning run with `--certificate <dir>` writes two TSV files:

- `asserted.tsv` holds one input triple per line, in N-Triples spelling.
- `derivations.tsv` holds one line per inferred triple: the rule name, the
  conclusion and the premises the rule read.

The fixtures live in `tests/fixtures/horn/`. A checker in `lean/` (command
`lake exe oo-cert asserted.tsv derivations.tsv`) re-derives every line against
a formal semantics. Its soundness is a machine-checked theorem
(`OOCert.certificate_sound`). A forged line makes it exit 1 and name the rule
that does not hold. The design is in `docs/decisions/0002-*.md`, and the
trusted parts are listed in `docs/trusted-computing-base.md`.

**Why it matters for HornDB.** We already record `(rule_id, premises)` for
each derived triple. An exporter from `Engine::proof` to this format would give
the harness an independent soundness check that shares no code with our engine.
Run it over the W3C OWL 2 RL selection and LUBM-1.

**Coverage limits.** The Lean rule table (`lean/OOCert/Rules.lean`) covers RDFS
plus about 23 OWL 2 RL rules: `cls-avf`, `cls-hv1/2`, `cls-int1/2`, `cls-oo`,
`cls-svf1`, `cls-uni`, `eq-sym`, `prp-inv1/2`, `prp-symp`, `prp-trp` and
`scm-*`. It does not cover `eq-rep-*`, cardinality (`cls-maxc*`, `cls-maxqc*`),
keys (`prp-key`), functional properties (`prp-fp`, `prp-ifp`) or the datatype
rules (`dt-*`). A line that uses one of those rules would be reported as
unchecked, not as wrong. Our GraphBLAS closure rows record empty premises, so
they would have to be expanded into rule steps first, or left unchecked.

**Toolchain cost.** Lean 4 is not Rust. Its runtime and kernel are C and C++,
and the rest is written in Lean itself. The checker has no Lean dependencies
(no Mathlib) and builds with `lake build` on the toolchain pinned in
`lean/lean-toolchain`, which `elan` installs. It would run as a separate binary
in CI. It adds nothing to the Cargo tree.

### F2. Semantic diff of a schema change (learn)

Their headline feature is `plan`: give it a proposed ontology and it reports
the consequences that the change adds, not only the text diff. Their example is
one added `rdfs:domain` triple. A text diff shows one line and a structural
diff shows nothing at risk, yet the change adds 901 inferred triples
(`src/plan.rs`, `src/conservativity.rs`, `src/closure_diff.rs`). HornDB's DBSP
circuit and change feed could offer the same as a dry-run transaction, at a
scale their full recompute cannot reach. This is a product idea, not code to
port.

### F3. Reports say which rules ran (learn)

Their first LUBM run used `owl-rl` and answered 8 of 14 queries fully. The
cause was not a bug. LUBM defines `Student` and `Chair` by equivalence to an
existential restriction, and plain OWL 2 RL does not reason over that. An
incomplete answer looks the same whether the engine failed or was never asked
to do the work. Their reports name the rule profile that ran. Their verdicts
also change wording when a user-supplied rule took part
(`entailed_under_supplied_rules` instead of `entailed`). HornDB results should
state the active rule set in the same way.

### F4. SHACL (learn, only if SHACL reaches the roadmap)

HornDB has no SHACL. Their evaluator (`src/shacl.rs`) has a Lean
specification, writes W3C `sh:ValidationReport` graphs, and has a test that
fails if the number of W3C SHACL cases passed goes down. It is built on
Oxigraph and terms held as strings, so a port would be a rewrite.
`docs/decisions/0018-conformance-is-not-truth.md` is worth reading for its
split between engine defects and limits of the SHACL standard.

### F5. MCP as the distribution channel (learn)

MCP is how open-ontologies reaches users: agents call its reasoning, planning
and validation tools directly. HornDB has no MCP surface. A thin MCP server
over SPARQL query, update and proof retrieval would be cheap to build, and it
fits the SPEC-08 boundary.

## Not worth taking

- The SHIQ tableaux reasoner, the EL classifier, alignment, data import and
  embeddings are outside HornDB's scope.
- The storage and the forward-chaining loop. They are built on Oxigraph and
  sets of string triples, which is slower than our compiled rules and columnar
  storage.

## Suggested order

Start with F1. It is about a day of exporter work and gives the harness an
external soundness check. F2 is the strongest product idea and should be filed
as a Worklode task. F3 is a small reporting change. F4 and F5 depend on
roadmap decisions.
