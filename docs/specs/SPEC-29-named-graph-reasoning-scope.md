---
status: draft
date: 2026-07-28
scope: "SPEC-29 — what OWL 2 RL reasoning is applied to in a many-named-graph store, and where the derived triples go"
---

# SPEC-29 — Named-graph reasoning scope

**One-line thesis:** HornDB's storage knows about named graphs and its reasoner
does not, so nothing in the project says *what set of triples a rule fires over*
or *which graph the conclusion belongs to*. This spec settles both: reasoning
runs over a **declared reasoning view** (a shared vocabulary spine plus one data
graph), and every derived triple lands in that view's own inferred named graph,
never in the graph it was derived from.

**Refines:** SPEC-04 (rule engine — what the engine reads), SPEC-06 / SPEC-24
(the delta unit and per-view maintenance), SPEC-02 / SPEC-25 (the quad-aware
store the views read). **Configured through** SPEC-26. **Constrains** SPEC-27
(proofs must name the graph a premise came from) and SPEC-28 (dataset
semantics — see "Resolved with SPEC-28"). **Tracking:** `#TODO`.

**Depends on SPEC-28** for the `GRAPH` keyword, `FROM` / `FROM NAMED` dataset
construction, the Graph Store Protocol (GSP), and the default-graph-when-
unspecified decision. This spec designs none of those; it states the invariants
they must preserve.

**Depends on SPEC-30** (change-feed materializer: apply, cursor, recovery;
tracking `#TODO`) for applied-state durability, startup cursor reconciliation,
and rebuild-from-zero. P1 leans on rebuild-from-feed as the recovery story;
SPEC-30 is what makes that story real.

## Problem — reasoning is graph-blind, top to bottom

Storage is quad-aware. `Store::insert_quads` / `retract_quads` take a `GraphId`,
`MemoryTier` keys partitions by graph, and the N-Quads loader routes each quad to
its named graph (SPEC-02 F7). Nothing above storage carries that through:

- **The rule engine loads the default graph and silently drops the rest.**
  `Engine::load` skips every quad whose graph name is not the default graph
  (`crates/owlrl/src/integration.rs:196`). A store full of named graphs reasons
  to an empty closure and reports no error.
- **The rule engine has no concept of a graph.** `TripleStore`
  (`crates/owlrl/src/store.rs:15`) is `scan_predicate` / `probe` /
  `insert_inferred` over `Triple` = (s, p, o). `Provenance.premises` is a list of
  the same. There is no slot in which a graph could be recorded.
- **Inferences are indistinguishable from asserted data.**
  `load_with_reasoning` (`crates/sparql/src/exec/horn.rs:141`) runs the engine,
  then loads `materialized_triples()` — asserted base *plus* everything inferred,
  as flat lexical triples — into the columnar backend. After that call nothing
  can tell the two apart.
- **The incremental delta unit is a triple.** `TripleId = (u64, u64, u64)`
  (`crates/incremental/src/types.rs:6`); `DeltaRecord.triple` is that type. The
  change feed cannot say which graph a change belongs to.

So the question this spec answers has no answer in the code today, and the
answer cannot be bolted on later: it decides the shape of the rule engine's
input, the identity of a derived triple, the key of the Z-set, and the premise
record in a proof. Those are exactly the four places a retrofit is expensive.

## The workload this is designed for

The consuming data platform's shape (see ADR-0016, which already records the
downstream constraint):

- Thousands of small named graphs — one per Iceberg dataset descriptor, per
  research scope, per project. Order 5,000 graphs of a few thousand triples.
- A handful of shared vocabulary graphs — DCAT-3, CSVW, PROV-O, SKOS, SSSOM, and
  an in-house `dcat-si:` — that most data graphs depend on.
- Writes are whole-graph PUT with a **server-side diff**: the writer reads the
  graph's currently visible quads, diffs the parsed payload against them, and
  commits `adds` / `dels`.
- The graph IRI is meaningful data: for dataset descriptors it *equals* the IRI
  of the resource being described.

That is the classic **one shared TBox, many small ABoxes** shape. Two things
follow immediately, and they pull in opposite directions:

1. A data graph on its own contains no vocabulary, so reasoning it in isolation
   derives almost nothing. It must see the vocabulary.
2. Data graphs belong to different projects with no relationship to each other,
   so one project's assertions must not entail anything about another's.

Any scope that satisfies both must be *per data graph*, and must include the
vocabulary.

## Non-goals

- **Branches, tags, and as-of reads.** SoR-owned (ADR-0016); not HornDB's role.
- **Authentication and authorization.** Terminated upstream; HornDB is
  cluster-internal.
- **The SPARQL named-graph surface itself.** `GRAPH`, `FROM` / `FROM NAMED`, GSP,
  and default-dataset construction are SPEC-28's, tracked under the SPEC-07
  Stage-2 epic [#189](https://github.com/sunstoneinstitute/horndb/issues/189).
- **Proof persistence.** SPEC-04 F4 / epic
  [#188](https://github.com/sunstoneinstitute/horndb/issues/188). This spec adds
  a *graph* dimension to proofs; it does not decide where proofs are stored.
- **Rule-set completeness.** Which OWL 2 RL rules exist stays SPEC-04.
- **Multi-graph reasoning policies beyond the declared view.** No per-graph rule
  subsets, no per-graph rule priorities.

## Decisions

| # | Decision | Rationale in one line |
|---|---|---|
| D1 | Reasoning scope is a **declared reasoning view**: a named set of member graphs. | The scope has to be an operator's explicit statement, not an accident of what got loaded. |
| D2 | The shipped default template is **one view per data graph, each including the shared vocabulary spine**. | The only option that both sees the TBox and keeps projects apart. |
| D3 | The spine closes **once**, and each view reasons over `spine-closure ∪ data graph`. | Exact, not an approximation, for a monotone rule set — and it is what makes D2 affordable. |
| D4 | Derived triples land in a **per-view inferred named graph** under a reserved namespace. | Keeps the source graph byte-identical to what was PUT, and makes asserted-vs-inferred a graph name. |
| D5 | **A read of a source graph never returns derived quads**, whatever the query dataset is configured to be. | A PUT-diff against a graph containing inferences deletes data the client never had. |
| D6 | Inferred graphs are **not in the default dataset** unless config says so. | Same reason as D5, plus SPEC-27's isolation rule; the knob exists for read-only reasoned endpoints. |
| D7 | The **input** delta unit is the quad; the **derived** delta unit is `(view, triple)`. One circuit per view, one for the spine. | Keeps the Z-set key at three ids, and makes per-view maintenance independent by construction. |
| D8 | A proof records the **graph of every premise** and the **view** that produced the conclusion. | "Where did this come from" is unanswerable without it, and SPEC-27's node hashing breaks without it. |
| D9 | Reasoning scope is **server-scoped SPEC-26 config**, never a per-query override. | It changes what is materialized; a query cannot be allowed to redefine it. |

### D1/D2 — the choice of scope

Four options were on the table. Each is stated with what it costs and what it
gets wrong.

**(a) Reason over the union of all graphs.** One store, one closure. Logically
sound — OWL 2 RL is monotone, so the union entails a superset of what any subset
entails — but wrong for this workload in three ways. Every project's data can
entail facts about every other project's: one graph asserting
`owl:sameAs` or a functional-property axiom rewrites conclusions in graphs it has
nothing to do with. One graph that entails `owl:Nothing` makes the whole store
inconsistent. And any write to any of 5,000 graphs dirties one shared closure, so
there is no unit of maintenance smaller than everything. Cheapest in absolute
memory; fails the isolation the workload requires.

**(b) Reason per graph in isolation.** Each named graph closes on its own. Sound,
trivially isolated, and useless here: a descriptor graph does not contain
`dcat:Dataset rdfs:subClassOf dcat:Resource`, so `cax-sco` never fires and the
closure is near-empty. This option is *incomplete relative to the intended
entailment* — it answers a different question than the user asked.

**(c) Reason over (vocabulary graphs ∪ one data graph), per data graph.** Sees
the TBox, keeps projects apart. This is already the position ADR-0016 records for
local embedded use: "hydration is always `touched-graphs + spine` (TBox +
identity graph), because named graphs are a provenance unit, not a reasoning-
locality boundary." Its naive cost is the problem — see D3 and the cost model.

**(d) A declared dataset / "reasoning view" abstraction.** A named, configured
object with a member-graph selector and an output target. (a), (b), and (c) are
all expressible as views.

**Position: (d) is the mechanism, (c) is the shipped default.** A view is the
only thing that makes the scope reviewable — an operator can read the config and
say which graphs entail what. Shipping (c) as the default template means the
normal deployment needs no per-view configuration at all: declare the spine,
and every other graph gets a view.

A **reasoning view** is:

- a **view id** (stable, derived from the source graph IRI by D4's minting rule),
- a **member set**: the spine (D3) plus exactly one *source graph*,
- an **output**: the view's inferred named graph (D4), or nothing (virtual —
  deferred, see Phasing).

The default template is: *spine = the configured vocabulary graphs; one view per
graph that is not a spine graph and not reserved.*

### D3 — the spine closes once

For a monotone rule set `T` (OWL 2 RL bodies are negation-free — SPEC-24's
non-goals state this), with spine `S` and data graph `D`:

```
lfp(T, S ∪ D)  ==  lfp(T, lfp(T, S) ∪ D)
```

This is an **equality, not an approximation**: `S ∪ D ⊆ lfp(T,S) ∪ D` gives ⊇,
and `lfp(T,S) ⊆ lfp(T, S ∪ D)` with idempotence of the fixpoint gives ⊆. So the
spine's closure can be computed once, held once, and reused by every view without
changing any view's answer.

Consequences:

- The spine's derived triples are stored once, in a shared **spine-closure
  graph**. A view materializes only the triples it derives *beyond* the spine
  closure.
- **The spine is shared join state, not just shared storage.** De-duplicating
  spine *storage* is not enough: a view's circuit must read the spine as a
  **shared, read-only, indexed relation**, never as streamed circuit input.
  `Circuit` keeps `asserted_base`, `extent`, and `rule_weights` per instance
  (`crates/incremental/src/circuit.rs`), so feeding the spine in as input would
  index it once per view — order 10⁵ tuples × 5,000 views, tens of GB, which
  dwarfs both the per-view fixed overhead and the derived triples the cost model
  counts. This is a requirement on the P1 design, not an optimization: if the
  shared relation is not achievable, P4's lazy view instantiation becomes a P2
  prerequisite instead.
- A data graph is free to assert TBox axioms of its own (a project-local
  `rdfs:subClassOf`). Those fire inside that view and nowhere else — which is the
  isolation property, working as intended, not a limitation.
- The factoring is void the moment two views disagree about the spine. A view's
  member set must name the spine version it closed against, so a stale view is
  detectable rather than silently mixed.

**Two conditions the factoring depends on.** The identity is the standard
closure-operator law `cl(S∪D) = cl(cl(S)∪D)`, and HornDB's OWL 2 RL is a
monotone, inflationary, idempotent closure today. Two things keep it that way,
and both must hold:

1. **Full `owl:sameAs` materialization, with no representative canonicalization
   of stored triples.** The engine materializes the whole symmetric/transitive
   `sameAs` pair set; the union-find representative in
   `crates/closure/src/sameas.rs` is internal state, not output shape. If SPEC-04
   ever adopts representative-based `sameAs` compression — the obvious mitigation
   for the eq-rep skew this spec cites below — the canonical representative
   becomes scope-dependent, so closing `S` first can pick a different
   representative than closing `S ∪ D`, and the factoring breaks silently. Record
   this as an invariant D3 depends on.
2. **A view must say what it does with an inconsistency.** Monotonicity makes
   inconsistency *propagation* sound — the engine derives `owl:Nothing`
   membership rather than retracting or halting — but a view that derives it must
   surface that as a **per-view flag in the view catalog**. An inconsistent
   *spine* sets the flag on every view at once, which is the honest reading and
   the reason the flag is per view rather than per store.

### D4 — where inferences land

Options and their consequences:

- **In the source graph.** Rejected, hard. The platform's writer reads the
  graph's visible quads and diffs the payload against them. If inferences sit in
  graph `G`, the next PUT of `G` computes `dels` covering every inferred quad,
  and the SoR records deletions of triples the client never sent. HornDB then
  re-derives them, and the change feed enters a permanent churn loop. This is
  data loss with a feedback loop attached.
- **One global inferred graph.** Rejected. It loses per-source attribution, makes
  retracting one view's consequences a whole-graph problem, and re-introduces the
  cross-project coupling of option (a) on the output side even when the input
  side is isolated.
- **A per-view inferred graph.** Chosen. Retraction, staleness, and attribution
  are all per-graph operations. Asserted-vs-inferred becomes a graph name, which
  is exactly the distinction a consumer mirroring HornDB into another store
  already reads off a quad.
- **Virtual / backward-chained.** Not rejected in principle — a per-view dataset
  is small, which makes backward chaining *more* attractive here than over a
  global store. Out of the first slice; see Phasing.

**Minting the inferred graph IRI.** The source graph IRI equals a real resource
IRI, so it must not be mangled into another IRI that could collide with data.
Inferred graphs are minted under the reserved HornDB namespace, with the source
IRI carried as an opaque percent-encoded segment:

```
https://horndb.io/graph/inferred/<percent-encoded-source-graph-IRI>
https://horndb.io/graph/spine-closure
https://horndb.io/graph/views          # the view catalog (below)
```

The reserved-namespace hygiene rule of SPEC-27 F6 extends to these: loading,
`CREATE`-ing, or PUT-ing a graph under `https://horndb.io/graph/` is an error,
not a silent shadowing.

**The view catalog.** A reserved graph `https://horndb.io/graph/views` holds one
node per view describing its source graph, its spine members, its inferred graph
IRI, the spine version it closed against, and its freshness. This is how a client
discovers the inferred graph for a resource without guessing at the encoding, and
how an operator sees which views are stale. It is read-only, on the same terms as
SPEC-27's provenance view.

### D5/D6 — visibility

**D5 is a hard invariant, and it is the most important line in this spec:**

> The quad set returned when reading the contents of a source graph — through
> GSP, through `GRAPH <g> { ?s ?p ?o }`, or through any other path — is exactly
> that graph's asserted quads. Derived quads are never among them, regardless of
> how the query dataset is configured.

Because inferred triples live in a different graph (D4), this invariant holds by
construction rather than by filtering. That is the whole point of D4.

This settles SPEC-28 S3's base-vs-base+derived parameter: graph-scoped reads of a
source graph are always base-only; derived data is reached only by naming a
derived graph or through the D6 flag's dataset composition.

**D6 — default visibility.** A query that names no dataset does **not** see
inferred graphs. `SELECT ?s ?p ?o WHERE { ?s ?p ?o }` returns asserted data only —
whatever SPEC-28's default-dataset mode composes out of the non-reserved graphs,
and nothing derived — matching SPEC-27's isolation criterion. A consumer opts
in by naming the graph (`FROM` / `FROM NAMED`, SPEC-28), or an operator opts the
whole endpoint in with `reasoning.default_dataset_includes_inferred = true`.

Two clauses make that precise:

- **Naming is the opt-in, enumeration is not.** A reserved graph is always
  addressable by explicit name (`FROM NAMED <g>`, ground `GRAPH <g>`). It
  enumerates under `GRAPH ?g { … }` only when
  `default_dataset_includes_inferred` is set. Without that clause D6 and
  `GRAPH ?g` contradict each other, since `GRAPH ?g { ?s ?p ?o }` with no dataset
  clause *is* a query naming no dataset. This matches SPEC-28's no-dataset
  named-graph set.
- **The flag adds the spine closure too.** Views do not replicate spine-derived
  triples (D3), so a flag that added only the per-view inferred graphs would miss
  every TBox-derived triple. `default_dataset_includes_inferred = true` adds the
  per-view inferred graphs **and** `https://horndb.io/graph/spine-closure`.

The honest trade: a client that just wants "the reasoned view" gets nothing until
someone sets that flag or names the graphs. The recommended deployment is
explicit — an endpoint serving reasoned reads sets the flag, **and that endpoint
must not be the source a mirror diffs against.** Mirroring reads the asserted
graphs; reasoning reads the inferred ones; the two are different requests to the
same server, and D5 is what keeps them from being confused.

### D7 — the delta unit

- **Input side: the quad.** The change feed the platform emits is
  `{adds: [[g,s,p,o]…], dels: [[…]]}`. That maps directly onto SPEC-25 S1's
  `retract_quad_batch` / `insert_quads`, which already take a `GraphId`. Apply
  must be idempotent — adding a present quad and deleting an absent one are both
  no-ops with an observable count — because delivery is at-least-once and
  consumers replay on restart. **SPEC-28 S6 owns that store-boundary contract**
  (SPEC-25 S1 already requires it for retraction); this spec consumes it and does
  not redefine it.
- **Derived side: `(view, triple)`.** One `Circuit` per view, plus one for the
  spine. The graph is resolved at *routing* time — an incoming quad `(g,s,p,o)`
  is routed to every view whose member set contains `g` — so the Z-set key stays
  `TripleId = (u64,u64,u64)` and the DBSP operator state SPEC-24 S1 landed
  ([#210](https://github.com/sunstoneinstitute/horndb/issues/210)) does not widen.
  A view circuit reads the spine closure as D3's shared read-only indexed
  relation; the only spine tuples it ever ingests as input are the *deltas* of a
  spine change (below), never the spine's steady-state contents.
- **`DeltaRecord` grows a graph field**, so the change feed and the provenance
  resolver can attribute a change to a graph without re-deriving it. This is a
  breaking change to `crates/incremental`'s public record type and should land
  before SPEC-24 S3's feed rework
  ([#212](https://github.com/sunstoneinstitute/horndb/issues/212)) creates
  external subscribers, for the same reason S3 gives.

**Per-view maintenance is independent by construction.** A data-graph edit routes
to exactly one view circuit; no other view does any work. This is the property
that makes the whole design affordable, and acceptance criterion 5 measures it
directly.

**Spine change is the expensive case, and it is expensive irreducibly.**
Retracting `dcat:Dataset rdfs:subClassOf dcat:Resource` invalidates derivations in
every dependent view. The work is:

1. Re-derive the spine closure incrementally, using the delta-incremental
   retraction that already landed for rules
   ([#210](https://github.com/sunstoneinstitute/horndb/issues/210)) and for
   closure ([#211](https://github.com/sunstoneinstitute/horndb/issues/211)). This
   yields a spine delta `{adds, dels}`.
2. Fan that delta out as an *input delta* to every dependent view circuit,
   through the same delta-incremental path — not as a rebuild.

Total cost is proportional to the number of affected derived triples summed over
all views. There is no way under that bound: those triples genuinely have to
disappear. What the design buys is that the shared TBox work happens once, that
unaffected views do zero work, and that the fan-out is a bounded, resumable
queue rather than a stop-the-world rematerialization. The fan-out is rate-limited
so a vocabulary edit cannot stall the change-feed apply loop; views converge
asynchronously and report per-view lag.

### D8 — provenance

SPEC-27 makes proofs a queryable `hprov:` view. A proof that cannot say which
graph a premise came from is not usable in a store where the graph *is* the
provenance unit. Three additions, all of which constrain SPEC-27:

- **`hprov:graph`** on `hprov:Statement` — the graph the triple is in. For a
  premise this answers "which graph did this come from"; for a conclusion it is
  the view's inferred graph.
- **`hprov:view`** on `hprov:Derivation` — the view that produced it, joining to
  the view catalog.
- **Statement node identity hashes the canonical N-Quads form, not N-Triples.**
  Hashing the N-Triples form makes the same triple asserted in two graphs
  collide, so a proof would attribute a premise to the wrong source. SPEC-27's
  "Node identity" section carries the N-Quads rule (default-graph statements hash
  the line without a graph label, so their node identities are unchanged). It was
  changed there before implementation because it is a breaking change to a
  published node-naming scheme afterwards.

**The graph must be recorded, not reconstructed.** A triple can be present in
several member graphs of one view, so reconstructing "which graph did this premise
come from" from the view's member set is ambiguous. `Provenance.premises`
therefore carries the graph per premise, which means owlrl's `Triple` gains a
graph slot on the premise-recording path (not necessarily on the join hot path —
the implementation plan settles whether the rule engine's working set stays
triple-shaped with a side map).

**Cross-view proof walking.** A view's derivation may rest on a premise that is
itself derived, in the spine closure. The provenance resolver must be able to
follow `hprov:premise/^hprov:conclusion` from a view's derivation state into the
spine's. If it cannot, the derivation must report `hprov:premisesComplete false`
(SPEC-27 F5) rather than look like a leaf.

### D9 — configuration surface

A new `[reasoning]` section in SPEC-26's `ServerConfig`, resolved through the
existing layering (built-in defaults < `config.toml` < `config.d/*.toml` < env <
argv) with no new mechanism.

| Key | Type / default | Reload |
|---|---|---|
| `reasoning.enabled` | bool, `false` | restart-only |
| `reasoning.spine` | list of graph IRIs / IRI-prefix patterns, empty | restart-only (slice 1) |
| `reasoning.views.select` | `"all-except-spine"` (default) or a list of IRI-prefix patterns | restart-only (slice 1) |
| `reasoning.views.include_spine` | bool, `true` | restart-only |
| `reasoning.views.output` | `"graph"` (default) or `"none"` | restart-only |
| `reasoning.default_dataset_includes_inferred` | bool, `false` | hot |
| `reasoning.fanout.max_concurrent_views` | integer, `4` | hot — **P2** |
| `reasoning.fanout.batch_size` | integer, `1000` | hot — **P2** |

The `fanout.*` keys land with P2. P1 has no incremental fan-out, so in slice 1
they would configure nothing.

Validation, on top of what SPEC-26 S1 already does:

- A graph IRI matched by both `spine` and `views.select` is a fatal startup
  error naming both keys.
- A pattern matching the reserved `https://horndb.io/graph/` namespace is a fatal
  startup error.
- `reasoning.enabled = true` with an empty `spine` is accepted but logs that every
  view will derive only from its own graph (option (b) above) — a legal but rarely
  intended configuration.

**None of these keys is per-query overridable.** SPEC-26 S4's whitelist must not
grow to include them: a query that could redefine the reasoning scope would
change what is materialized for every other query.

**Backward compatibility.** With `reasoning.enabled = false`, behaviour is
exactly today's. With it enabled on a store that has no named graphs, the
degenerate single view over the default graph must be behaviourally identical to
today's `load_with_reasoning` — this is what keeps the SPEC-01 conformance
harness meaningful.

## Cost model

Order-of-magnitude **estimates**, not measurements. Assumed corpus: 5,000 data
graphs averaging 3,000 triples (≈15 M asserted triples), plus 5 vocabulary graphs
totalling on the order of 50 K triples. SPEC-04 NF2 budgets OWL 2 RL expansion at
≤4× the asserted set. Numbers below are for sizing the design; the slice-1 bench
(acceptance 7) replaces them with real ones.

**This is a sizing ceiling, not near-term reality.** Descriptor graphs run on the
order of 10² triples and the realistic near-term corpus is 10²–10³ graphs — two
orders below the assumption. The 50 K spine figure is generous by perhaps 2–5×
(PROV-O + SKOS + DCAT-3 + CSVW + SSSOM + `dcat-si:` is nearer 10–20 K asserted),
though the closure's order of magnitude is right. Keep the ceiling for sizing,
but **5,000-scale consequences must not gate the P1 slice.**

| Option | Derived triples (estimate) | Maintenance unit | Verdict |
|---|---|---|---|
| (a) Global union | one closure over 15 M ⇒ order 45–60 M | the whole store | Scales in absolute size, fails isolation. Any write dirties everything; `eq-rep-*` skew (`crates/owlrl` notes §7) and `rdf:type` skew (SPEC-04 F5) are at their worst across 5,000 projects' data pooled together. |
| (b) Per-graph isolation | ~0 | one graph | Cheap and useless — no TBox in scope. |
| (c) Vocabulary ∪ data, **no** spine factoring | 5,000 closures over ~53 K triples each ⇒ the spine's derived set replicated 5,000× | one graph | **Does not scale.** If the spine closes to ~100 K triples, that is ~500 M replicated derived triples before any data-specific derivation. This is the option to reject explicitly. |
| (c) + D3 spine factoring | spine ~100 K once, plus order 10³–10⁴ per view ⇒ order 15–50 M total | one graph | Same order as (a)'s closure, but partitioned and independently maintainable. Chosen. |

The cost the chosen option adds over (a) is **per-view fixed overhead**: 5,000
`Circuit` instances, each with operator traces and an incremental-`distinct`
weight trace (SPEC-24 S1). At 5,000 views, a per-view fixed cost of 100 KB is
500 MB of pure overhead; at 10 KB it is 50 MB. Which of those it is decides
whether views can all stay resident or must be lazily instantiated and evicted.
It is not knowable from the spec, and acceptance 7 exists to measure it before
slice 2 sets a budget.

**Per-view *variable* state is the larger hazard, and D3 is what bounds it.**
Fixed overhead is bounded by construction; spine join state is not. If each
view's circuit ingested `spine-closure ∪ data graph` as streamed input, every
circuit's `extent` and `rule_weights` would index the whole spine — order 10⁵
tuples per view, order 10⁹ trace entries at 5,000 views, tens of GB. That dwarfs
both the fixed overhead and the derived triples this table counts. D3's shared
read-only indexed spine relation is the requirement that keeps this off the
table, and acceptance 7 measures per-view spine-attributable state to prove it.

## Phasing

Each slice is independently shippable and harness-gated (the SPEC-01 selected
subset stays green throughout). Implementation plans (`PLAN-29-MM-*.md`) are
written when a slice is picked up; tracking issues are filed then (`#TODO` until
they are).

1. **P1 — the reasoning materializer slice.** The near-term H1 target: HornDB
   beside Oxigraph, fed by a `{adds, dels}` change feed per named graph. Contains:
   the view model and catalog (D1/D2), spine factoring (D3), per-view inferred
   graphs (D4), the D5 read invariant, quad-shaped input deltas with idempotent
   apply and per-view routing (D7), the `[reasoning]` config section (D9), and the
   `reasoning.enabled = false` no-op path. Spine changes mark every dependent view
   stale and re-derive it in the background, resumably — no incremental fan-out
   yet. This is acceptable because rebuild-from-feed is the platform's recovery
   story. **SPEC-30** (`#TODO`) is what makes that story real — it owns
   applied-state durability, startup cursor reconciliation, and
   rebuild-from-zero — so P1 rests on SPEC-30 here rather than assuming
   recoverability HornDB does not yet have. On the write path P1 needs **SPEC-28 S6**
   (store-boundary idempotent quad apply), which is landable independently of the
   rest of SPEC-28; it needs SPEC-28's `FROM NAMED` only for a client to query an
   inferred graph by name. *(tracking: `#TODO`)*
2. **P2 — incremental spine fan-out.** Replace slice 1's re-derive with the
   delta-incremental path (SPEC-24 S1/S2, landed), bounded and rate-limited, plus
   per-view lag and staleness metrics. Sets the per-view overhead budget from P1's
   measurement. *(tracking: `#TODO`)*
3. **P3 — provenance graph attribution.** `hprov:graph`, `hprov:view`, N-Quads
   statement hashing, cross-view proof walking (D8). The N-Quads hashing rule is
   already in SPEC-27, ahead of its implementation; this slice implements it
   rather than deciding it. *(tracking: `#TODO`)*
4. **P4 — virtual views and lifecycle.** `views.output = "none"` backed by
   backward chaining, lazy view instantiation and eviction under memory pressure,
   and the ADR-0016 capability-4 named-graph delta export. *(tracking: `#TODO`)*

P1 stands alone and delivers the near-term target. P2 depends on P1's view model.
P3 is independent of P2. P4 is the completeness tail.

## Resolved with SPEC-28

This spec once carried six open constraints on SPEC-28's dataset semantics.
SPEC-28 has settled all six; what each became is recorded here so the overlap
stays reviewable and nobody re-opens a closed question.

1. **GSP reads and the PUT diff see asserted quads only (D5).** Accepted and
   pinned in SPEC-28 S5: the PUT diff reads the graph's asserted (base) quads,
   with the reasoning seam held to base-only. Derived quads are never in the read
   set or the diff. This was the data-loss case, not a preference.
2. **The default dataset must not blend asserted and inferred.** Resolved by
   SPEC-28's reserved-namespace exclusion: its default is still the union of all
   graphs, but graphs under `https://horndb.io/graph/` are never in that union.
   They enter the default dataset only when
   `reasoning.default_dataset_includes_inferred` is set. D6 holds without SPEC-28
   changing its default mode.
3. **Derived graphs are nameable — no SPEC-28 change needed.** SPEC-28's
   "unknown graph IRI yields zero rows" rule plus its graph-name handling already
   make any quad-holding graph nameable by `FROM NAMED` / `GRAPH <g>`. Stated
   explicitly because it looks like an open item and is not one; there is nothing
   to fix here.
4. **`GRAPH ?g { … }` enumeration.** Resolved: reserved graphs are always
   addressable by *explicit* name, and enumerate under `GRAPH ?g` only when
   `default_dataset_includes_inferred` is set (D6). This replaces this spec's
   earlier "they always enumerate" position, which contradicted D6.
5. **The reserved namespace is closed to writes.** Accepted into SPEC-28 S4/S5:
   any write targeting `https://horndb.io/graph/…` — `CREATE`, `LOAD INTO`,
   `INSERT DATA`, GSP `PUT`/`POST`/`DELETE` — is an error naming the namespace,
   not suppressible by `SILENT`. GSP `GET` of a reserved graph stays allowed.
   This extends SPEC-27 F6 from the provenance graph to all reserved graphs.
6. **Graph lifecycle cascades.** Accepted into SPEC-28 S4: `CLEAR` / `DROP` of a
   source graph retracts its quads through the same store boundary as
   `DELETE DATA`, so the retraction flows through the delta path and each view
   withdraws the derived triples that rested on it. `CLEAR` / `DROP` of a
   reserved graph is an error (constraint 5), on the same terms as SPEC-27 F7.

## Acceptance criteria

1. **Scope is exactly the declared view.** For a fixture spine `S` and data graph
   `G`: `S ∪ G ∪ spine-closure-graph ∪ view-inferred-graph == lfp(T, S ∪ G)` as
   triple sets, pinned by a differential test against a single-store `Engine`
   load of `S ∪ G`, over every graph in the fixture set. The check is over the
   union, not over the view's inferred graph alone, because D3 keeps
   spine-derived triples out of that graph by design.
2. **Isolation holds.** With two data graphs `G1`, `G2` that *would* entail across
   each other under a global union (e.g. an `owl:sameAs` in `G1` naming a subject
   in `G2`), neither view derives anything that depends on the other's data. Zero
   cross-entailment, asserted by test.
3. **Spine factoring is exact.** `lfp(T, S ∪ D) == lfp(T, lfp(T, S) ∪ D)` as sets,
   over the rule shapes exercised by `harness/curation/owl2-rl-50.md`.
4. **The PUT round trip is lossless (D5).** After materialization, reading graph
   `G` returns exactly the quads last written to it — quad-set equality — and a
   write / read / write round trip produces an empty second diff. This is the
   data-loss test and it runs against the real read path, not a unit stub.
5. **Default-dataset isolation (D6).** With
   `default_dataset_includes_inferred = false`, `SELECT ?s ?p ?o WHERE { ?s ?p ?o }`
   returns exactly the triples it returns with reasoning disabled, and
   `GRAPH ?g { ?s ?p ?o }` binds no reserved graph — while a ground
   `GRAPH <inferred-g>` still answers. Flipping the flag adds exactly the per-view
   inferred graphs **and** `https://horndb.io/graph/spine-closure`, and nothing
   else.
6. **Per-graph updates are per-graph work.** Applying `{adds, dels}` for one data
   graph performs zero derivation work in every other view, shown by a per-view
   derivation counter. Re-applying the identical batch is a no-op (idempotent
   apply), and both an add of a present quad and a delete of an absent quad
   complete with an observable count of zero.
7. **Cost is measured, then budgeted.** On a synthetic 5,000-graph / ~15 M-triple
   corpus on `hornbench`: record resident memory, **per-view fixed overhead**,
   **per-view spine-attributable state** (which must stay flat as view count
   grows — the D3 shared-relation requirement, and the number that falsifies it),
   total derived-triple count, and single-data-graph update latency in
   `docs/benchmarks.md` with the host noted. Single-graph update visibility meets
   SPEC-06 NF1's 100 ms. The per-view overhead budget is set from this
   measurement in the P2 plan, not invented here. This bench shares one synthetic
   corpus with SPEC-28's phase-2 partition-overhead bench.
8. **Spine change converges (P1) and is incremental (P2).** Retracting one
   `rdfs:subClassOf` from a vocabulary graph withdraws the dependent derived
   triples in every dependent view and in no other view. In P1, the re-derive
   converges and survives a restart mid-fan-out. In P2, total work is proportional
   to affected derivations, pinned against the P1 re-derive as the differential
   oracle.
9. **Config behaves (D9).** `[reasoning]` resolves through SPEC-26's layering
   (verified for file < env < argv); no `reasoning.*` key is accepted as a URL
   query-parameter override; a graph matched by both `spine` and `views.select`,
   and a pattern matching the reserved namespace, each fail startup with a message
   naming the key.
10. **Harness-first, and the degenerate case is unchanged.** The SPEC-01 selected
    subset stays green with reasoning views enabled. On a corpus with no named
    graphs, the single default-graph view produces a byte-identical closure to
    today's path.

## Open items

- **Per-view fixed overhead is the design's load-bearing unknown.** If a
  `Circuit`'s fixed cost is closer to 100 KB than 10 KB, 5,000 resident views are
  not viable and P4's lazy instantiation becomes a P2 prerequisite rather than a
  tail item. Acceptance 7 forces the number into the open early. The *variable*
  half of the same question — per-view spine join state — is not left open: D3
  requires the spine to be a shared read-only indexed relation, and acceptance 7
  measures whether that requirement was met.
- **`Triple` vs. quad in the rule engine's hot path.** D8 requires the graph on
  the premise record. Whether that means widening `owlrl::Triple` (touching every
  generated rule body and the join hot path) or keeping a side map from the
  working triple to its source graph is an implementation-plan decision with a
  real performance consequence. Not settled here.
- **Views over more than one data graph.** The default template gives each view
  one source graph. A project that legitimately spans several graphs (a scope plus
  the descriptors it references) would want a multi-source view. The model
  supports it; the *selector syntax* for expressing it does not exist yet, and
  the cost of overlapping views (a graph in several views derives several times)
  is unquantified.
- **Spine versioning granularity.** D3 requires a view to record which spine
  version it closed against. Whether that is a single monotonic counter over all
  spine graphs or a per-spine-graph vector decides how finely staleness can be
  computed, and therefore how much of a fan-out can be skipped.
- **Interaction with `owl:sameAs` across the spine boundary.** If a vocabulary
  graph asserts `owl:sameAs` between terms, `eq-rep-*` substitution happens in the
  spine closure and every view inherits it — correct, but it means a vocabulary
  edit can change term identity everywhere at once. Worth a bench, because
  `eq-rep-p` skew is already the known worst case (`crates/owlrl` notes §7). Note
  that the obvious mitigation — representative-based `sameAs` compression — is
  the thing D3's first condition forbids, so it cannot be adopted without
  re-deriving the spine factoring.
- **Does the mirror-out consumer need a stable inferred-graph IRI across
  processes?** D4's minting is deterministic from the source IRI, so two HornDB
  processes over the same data agree. Whether the *downstream* store wants those
  IRIs verbatim, or wants them rewritten into its own namespace, is a data-platform
  question not settled here.
- **Backward-chained views (P4) change the cost model entirely.** For a workload
  whose queries are descriptor lookups over one small graph, answering from a
  per-view backward chase may beat materializing 5,000 closures outright. That
  should be re-evaluated with acceptance 7's numbers in hand rather than assumed
  either way.
