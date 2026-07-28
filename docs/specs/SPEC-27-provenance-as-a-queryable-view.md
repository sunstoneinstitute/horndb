---
status: draft
date: 2026-07-28
scope: "Expose OWL 2 RL proofs to users as an RDF view queryable with ordinary SPARQL — reserved provenance vocabulary, query-time resolution, recursion via property paths"
---

# Provenance as a queryable view

**Predecessors:** SPEC-04 F4 (proof recording — built), ADR-0013 (provenance is a
hard requirement), SPEC-11 F2/F8 (the n-ary claim-node precedent).
**Tracking task:** [#260](https://github.com/sunstoneinstitute/horndb/issues/260).

## Problem

HornDB records a proof for every triple it derives, and no user can see it.

The machinery works. Every compiled `rules.toml` rule and every hand-written
`list_rules.rs` rule records `Provenance { rule_id, premises }` per derived
triple. `MemStore::proof_tree` (`crates/owlrl/src/provenance.rs:28`) walks those
premises into a recursive `ProofTree` that bottoms out at asserted triples, and
`Engine::proof(s, p, o)` (`crates/owlrl/src/integration.rs:410`) returns the same
tree decoded to lexical IRIs. `crates/owlrl/tests/proof_tree.rs` gates it inside
the SPEC-04 NF4 100 ms budget.

None of it reaches a user. `load_with_reasoning`
(`crates/sparql/src/exec/horn.rs:141`) builds a local `owlrl::Engine`, dumps
`materialized_triples()` — plain lexical strings, no provenance — into the
columnar backend, and drops the `Engine` when the function returns. After
startup, a `serve --materialize` process holds the closure with no derivation
data in it at all. The axum router (`crates/sparql/src/server/mod.rs:50`) offers
`/query`, `/update`, `/metrics` and nothing else. The Python binding has no
reasoning surface. `docs/guides/getting-started.qmd:148` says so out loud, under
the heading "Proof tracking is not exposed yet".

For a project whose sixth bet is "provenance / correctability as a hard
requirement" (SPEC-00), that gap is the most conspicuous one in the product.

## Approach in one line

Make the proof an **RDF view over the reasoner's derivation state**, addressed by
a reserved vocabulary and answered at query time — so users read proofs with
ordinary SPARQL, and recursion through a derivation chain is a property path
rather than a bespoke tree format.

## Non-goals

- **A proof-tree wire format.** No JSON proof schema, no new result-format key,
  no `/proof` endpoint. If the proof is in the graph, SPARQL is already the API.
- **Proof persistence.** The compressed side-table with on-demand re-derivation
  stays SPEC-04 F4 / epic E4
  ([#188](https://github.com/sunstoneinstitute/horndb/issues/188)). This spec
  defines the *surface*; it consumes whatever retained state F4 provides and
  specifies the minimum retention the surface needs (F1).
- **Closing the two known elisions.** The GraphBLAS closure backend records
  empty premises by design, and restriction-rule schema declarations are an
  elided side condition. This spec does not fix either — it requires that both
  be **visible** to the querying user instead of silently resembling a complete
  proof (F5).
- **Retraction or correction workflows.** ADR-0013 names correctability as the
  downstream goal; reading proofs is the prerequisite, not the whole of it.
- **Writing to the view.** The provenance view is read-only. `INSERT`/`DELETE`
  touching the reserved namespace is an error (F7).

## Three constraints that shape the design

These were each verified against the tree, and each rules out an obvious option.

**1. The store is default-graph-only, so a provenance named graph is
unrepresentable today.** `GRAPH` patterns lower transparently to the inner
pattern and a graph-name variable stays unbound
(`docs/architecture.md:303`); the Graph Store Protocol is deferred precisely
because "named graphs are unrepresentable until SPEC-02 grows a quad-aware
seam" (`docs/architecture.md:319`). True scoping sits in epic E5
([#189](https://github.com/sunstoneinstitute/horndb/issues/189)). So the view
cannot be delivered as "just load the proofs into a second graph", and it must
not be dumped into the default graph either — that would make proof triples
answer `SELECT ?s ?p ?o WHERE { ?s ?p ?o }`.

**2. RDF 1.2 triple terms are the wrong carrier.** ADR-0014 makes RDF 1.2 a
Stage-2 priority and gates SPARQL triple-term patterns behind
`SparqlConfig::rdf12` (default off). Worse, the OWL 2 RL engine *explicitly
bails* on triple-term inputs — `intern_term` and `triple_entailed` refuse a
`TermRef::Triple` (`crates/owlrl/CLAUDE.md` §7). A triple-term proof encoding
would therefore produce triples HornDB's own reasoner cannot read back.
ADR-0016 makes the same call for the same reason: "claims are n-ary nodes in
RDF 1.1".

**3. The n-ary reification node is already the house form for this exact role.**
SPEC-11 F2 models each mapping as "an n-ary `sssom:Mapping` node (the canonical,
asserted form — same shape as a claim node, and as SSSOM's OWL reification)" and
calls it "the provenance/correctability unit (ADR-0013)". SPEC-11 F8 then
populates `derived_from` from SPEC-04 F4 proof premises. This spec adopts the
same shape rather than inventing a parallel one.

## Design

### The provenance view

A **virtual** RDF graph, resolved per query from retained derivation state. Its
triples are never inserted into the store: they have no `TermId`s, they do not
appear in `?s ?p ?o`, they do not count toward store size, and they cost nothing
when a query does not mention them.

A pattern is routed to the provenance resolver instead of the triple store when
its predicate is in the reserved namespace, or when a bound subject or object is
a reserved-namespace node (F3). Everything else is unchanged.

Namespace `https://horndb.io/ns/prov#`, conventionally bound to `hprov:`.

| Term | Shape | Meaning |
|---|---|---|
| `hprov:Statement` | class | A triple, reified so it can be talked about. |
| `hprov:subject` / `hprov:predicate` / `hprov:object` | `Statement` → term | The reified triple's three slots. |
| `hprov:Derivation` | class | One rule application that produced a triple. |
| `hprov:conclusion` | `Derivation` → `Statement` | What this rule application derived. |
| `hprov:rule` | `Derivation` → `xsd:string` | The W3C rule id, e.g. `"cax-sco"`. |
| `hprov:premise` | `Derivation` → `Statement` | An input triple of the rule application. Unordered, zero or more. |
| `hprov:premisesComplete` | `Derivation` → `xsd:boolean` | `false` when premises were not fully recorded — see F5. This is how an elision announces itself. |

An asserted (base) triple has a `hprov:Statement` and **no** `hprov:Derivation`.
That absence is the definition of a proof leaf, so no explicit `asserted` flag is
needed — `FILTER NOT EXISTS { ?d hprov:conclusion ?stmt }` is the test.

### Node identity

Statement and derivation nodes are skolem IRIs under the reserved namespace,
named deterministically from a fixed-width hash of the reified statement's
canonical **N-Quads** form:

```
https://horndb.io/ns/prov/stmt/<hash>
https://horndb.io/ns/prov/deriv/<hash>       # <hash> of the conclusion
```

**Why N-Quads and not N-Triples.** The graph a statement sits in is part of its
identity. Hashing the triple alone gives one node to the same triple asserted in
two graphs, so once premises carry their source graph (SPEC-29 D8) a proof would
attribute a premise to the wrong graph. A statement in the default graph hashes
its N-Quads line **without** a graph label — which is exactly its N-Triples
line — so default-graph node identities stay stable, including on today's
default-graph-only store (constraint 1).

Hashing the *lexical* form, not `TermId`s, is deliberate: dictionary ids are not
stable across loads, and a proof reference that changes meaning after a
re-materialization is worse than no reference. Deterministic names also let a
client join proof results across separate queries, and let two HornDB processes
over the same data agree on node names.

One consequence to note honestly: `MemStore` records at most one `Provenance` per
triple, so the conclusion hash uniquely names its derivation today. If SPEC-04
ever records *all* derivations of a triple, `deriv/<hash>` gains a discriminator
suffix, and clients must not assume one derivation per conclusion.

### Worked example — the getting-started case

`ex:Felix a ex:Mammal` derived by `cax-sco` from `ex:Felix a ex:Cat` and
`ex:Cat rdfs:subClassOf ex:Mammal`:

```sparql
PREFIX hprov: <https://horndb.io/ns/prov#>

SELECT ?rule ?ps ?pp ?po WHERE {
  ?stmt hprov:subject   ex:Felix ;
        hprov:predicate  rdf:type ;
        hprov:object     ex:Mammal .
  ?d hprov:conclusion ?stmt ;
     hprov:rule       ?rule ;
     hprov:premise    ?prem .
  ?prem hprov:subject ?ps ; hprov:predicate ?pp ; hprov:object ?po .
}
```

Two rows, both with `?rule = "cax-sco"`.

### Recursion is a property path

This is the payoff, and the reason the view beats any tree-shaped response
format. Stepping from a derivation to the derivations of its premises is
`hprov:premise/^hprov:conclusion`, so the whole proof tree below a conclusion is
that step under Kleene closure:

```sparql
SELECT ?rule WHERE {
  ?d hprov:conclusion ?stmt .
  ?d (hprov:premise/^hprov:conclusion)* ?step .
  ?step hprov:rule ?rule .
}
```

`translate_path` (`crates/sparql/src/algebra/translate.rs:592`) already lowers
`^` (Reverse), `/` (Sequence), and `*`/`+` — the latter to an
`Algebra::PathClosure` whose edge is the *inner path* expanded over hidden
endpoint variables, so a composite inner path works. Cycle cutting, which
`ProofTree::Cycle` needs in a materialized tree, comes free: `PathClosure`
evaluates a set-valued fixpoint, so a derivation cycle terminates rather than
recursing.

Users also get everything else SPARQL brings, none of which a bespoke proof
format would have: `COUNT` the rules in a proof, `FILTER` a derivation chain by
rule id, `CONSTRUCT` a PROV-O rendering, `ASK` whether a fact depends on a given
source triple.

## Functional requirements

**F1. Retained derivation state.** The reasoner's proof state must survive
`load_with_reasoning`. Minimum viable form: retain the `owlrl::Engine` (its
`MemStore` and dictionary) in the server state next to the columnar store, gated
by config (F8), so the memory cost is only paid when proofs are wanted. The
production form is SPEC-04 F4's compressed side-table with on-demand
re-derivation
([#188](https://github.com/sunstoneinstitute/horndb/issues/188)); this spec's
surface must not assume which of the two is underneath. Define the seam as a
read-only trait — given a triple, yield its derivation; given a derivation,
yield its premises — with the `Engine` as the first implementor.

**F2. Dictionary crossing.** The owlrl `Engine` has its own dictionary (lexical
keys, `USER_TERMS_BASE`) separate from `horndb-storage`'s `Dictionary`. The
resolver crosses that boundary on lexical forms. `Engine::proof` today rebuilds
the whole reverse dictionary per call (O(dict), documented as introspection-grade)
— that is unacceptable for a query operator, so the seam in F1 must expose a
persistent reverse map or id-level lookup instead.

**F3. Pattern routing.** A triple pattern is answered by the provenance resolver
when its predicate is an IRI in the reserved namespace, or when a bound subject
or object is a reserved-namespace skolem IRI. All other patterns go to the store
unchanged. Mixed BGPs — some patterns from the store, some from the view — join
normally; that is what makes "explain the answer I just got" a single query.

**F4. Bounded enumeration.** An unconstrained pattern such as
`?d hprov:rule ?r` with no bound endpoint enumerates every derivation in the
closure. Such patterns are permitted but subject to the SPEC-26 result-row cap,
and the resolver must stream rather than materialize the enumeration. Planner
cardinality estimates for view patterns come from the derivation count, not the
triple count.

**F5. Elisions are visible, never silent.** A `Derivation` whose premises were
not fully recorded must carry `hprov:premisesComplete false`. This covers both
documented cases: the GraphBLAS closure backend's best-effort empty premises, and
the restriction rules' elided schema side conditions (instance premises are still
recorded, so those trees still bottom out at asserted instance data). A user
walking a chain must be able to tell "this is a leaf" from "this is where our
recording stops".

**F6. Reserved-namespace hygiene.** Loading data that uses the reserved namespace
is a load-time error, not a silent shadowing of the view. The reserved graph IRI
`https://horndb.io/graph/provenance` is registered now, unused, so that the view
can migrate from virtual-in-default-graph to a real named graph when E5
([#189](https://github.com/sunstoneinstitute/horndb/issues/189)) lands, without
a vocabulary change.

**F7. Read-only.** An `INSERT`/`DELETE` template or `WHERE` clause that would
write a reserved-namespace triple fails with a clear error naming the namespace.

**F8. Opt-in.** The view is off by default and enabled through SPEC-26 config
(server-level, since it changes what load retains). When it is off, reserved-
namespace patterns return zero solutions and the error message says the view is
disabled — silently empty results would look like "this fact has no proof".

**F9. Metrics.** Emit derivation-state resident size, view-pattern evaluations,
and resolver latency. Per the root `CLAUDE.md` sync rule, every new series lands
in `docs/metrics.md` in the same commit as its emit site.

## Non-functional requirements

**NF1. Proof latency.** A single conclusion's one-level derivation resolves in
≤10 ms, and a full recursive walk of depth ≤10 stays inside SPEC-04 NF4's 100 ms
budget — the same bar `crates/owlrl/tests/proof_tree.rs` already holds, now
measured through SPARQL.

**NF2. Zero cost when unused.** A query mentioning no reserved-namespace term
shows no measurable regression against the same query before this spec. Gated by
the existing SPB-256 nightly, not by a new bench.

**NF3. Retention overhead is measured and documented.** Enabling the view has a
resident-memory cost (a second copy of the closure, in the F1 minimum form).
Measure it on `hornbench` over the LUBM load and record it in
`docs/benchmarks.md`, so operators can decide from a number rather than a guess.

## Dependencies

- **SPEC-04 F4** — the derivation state this view reads. The minimum retention
  form (F1) needs no new F4 work; the production form is E4
  ([#188](https://github.com/sunstoneinstitute/horndb/issues/188)).
- **SPEC-07** — the pattern-routing seam and the planner's cardinality estimates.
- **SPEC-26** — the config surface for F8.
- **SPEC-11** — shares the n-ary claim-node shape; a mapping's `derived_from`
  (F8 there) and a derivation's `hprov:premise` describe the same premise set and
  must not drift into two vocabularies for one concept.
- **E5 / [#189](https://github.com/sunstoneinstitute/horndb/issues/189)** — not a
  blocker, but the eventual home of the view as a real named graph (F6).

## Risks and open questions

- **Default-graph residency is a compromise, not the design.** Until named-graph
  scoping lands, the view's triples are addressable from the default graph even
  though they are not *in* it. F6's reserved-namespace error and reserved graph
  IRI are what keep that from becoming permanent, but a user's mental model will
  be slightly off until E5.
- **Memory is the real adoption gate.** If the F1 minimum form doubles resident
  footprint on a large closure, the view will be switched off in exactly the
  production settings where proofs matter most. NF3 exists to force that number
  into the open early; if it is bad, E4's side-table becomes a hard prerequisite
  rather than an upgrade path.
- **One derivation per triple is a current limitation, not a semantic claim.**
  See "Node identity". Deciding whether HornDB should record *all* derivations of
  a triple is SPEC-04 territory and is not settled here.
- **Open: does the view expose ML-derived provenance too?** SPEC-08 F5 requires
  ML-admitted triples to carry `prov:wasDerivedFrom` with generator identity and
  confidence, and `crates/ml/src/provenance.rs` exists. Unifying that with
  `hprov:` would give one explanation surface for symbolic and ML paths both.
  Deliberately deferred — it widens the vocabulary and needs SPEC-08 input.
- **Open: PROV-O alignment.** `hprov:` is deliberately minimal and HornDB-shaped.
  Whether to publish a standing PROV-O rendering (`prov:wasDerivedFrom`,
  `prov:Activity`) or leave it to a user `CONSTRUCT` is unresolved; PROV-O-backed
  graphs are a primary Sunstone workload (ADR-0013), so this will come back.

## Acceptance criteria

1. **The getting-started case is answerable in SPARQL.** The query in "Worked
   example" returns `cax-sco` and both premises against the guide's own dataset,
   through `/query` on a running `serve`, with no client-side proof parsing.
   `docs/guides/getting-started.qmd:148`'s "not exposed yet" section is replaced
   by that worked example.
2. **Recursion works.** For an N-step `rdfs:subClassOf` chain, the property-path
   walk returns every rule application between the conclusion and the asserted
   leaves, and terminates on a derivation cycle (`eq-sym` ↔ `eq-sym`).
3. **Parity with the built API.** For every derived triple in the curated
   `harness/curation/owl2-rl-50.md` subset, the view's derivation and premises
   agree exactly with `Engine::proof`'s `StringProofTree` for the same triple.
   `Engine::proof` is the differential oracle — the view must not become a second,
   divergent proof implementation.
4. **Elisions are visible.** A GraphBLAS-closure-derived triple reports
   `hprov:premisesComplete false`; a fully recorded compiled-rule derivation
   reports `true`. Both are asserted by test, under both closure backends.
5. **Isolation.** With the view enabled, `SELECT ?s ?p ?o WHERE { ?s ?p ?o }`
   returns exactly the triples it returned with the view disabled — no reserved-
   namespace triple leaks into ordinary query results. Loading data in the
   reserved namespace errors; an update touching it errors.
6. **Off by default, and loud about it.** With the view disabled, a reserved-
   namespace query reports that the view is disabled rather than returning zero
   solutions.
7. **Budgets hold.** NF1 measured through SPARQL; NF2 shows no SPB-256
   regression; NF3's retention overhead recorded in `docs/benchmarks.md` with the
   `hornbench` environment noted.
8. **Harness-first.** The conformance subset stays green with the view enabled
   (per the root `AGENTS.md` rule), and criterion 3's parity check runs as part of
   it rather than as a one-off.
