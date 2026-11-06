# ADR-0020: A runtime rule IR for backward chaining, emitted from `rules.toml`

**Status:** Proposed. This record asks Stig (CTO) for one decision: which of the four options in "Options considered" HornDB takes, how the recursive stratum executes (see the recommendation), and whether the recommended narrowing of ADR-0004 is acceptable. Nothing in it is accepted or implemented.

**Date:** 2026-09-19

**Source:** Written for Worklode task HDB-237 (escalated from HDB-217, the magic-sets task of PLAN-23-07 / HDB-PLAN-60). Realizes [ADR-0005](0005-hybrid-forward-backward-chaining.md) and narrows [ADR-0004](0004-compile-owl2rl-rules-ahead-of-time.md); see "Consequences".

## Context

ADR-0005 bets on a hybrid: materialize the schema and transitive-closure subset, backward-chain the rest with magic sets and SLG tabling (SPEC-03 F4/F5), and expose both through the SPARQL backward-chained entailment mode (SPEC-07 F4). PLAN-23-07 Task 1 (HDB-217) is where that starts: "transform the query goal + rules into demand-restricted rules so only demand-relevant facts are derived. Feed the result into the [PLAN-23-06] reasoning rewrite passes." The worker found that neither the input nor the target of that sentence exists.

**There is no runtime rule.** `horndb-wcoj` has no rule, atom, or adornment type; `crates/wcoj/src/lib.rs` says "Magic sets and SLG tabling are deferred." `horndb-owlrl` compiles `crates/owlrl/rules.toml` to Rust in `build.rs` (ADR-0004): one `fire_<id>` function per rule, and a `RULES: &[CompiledRule]` table holding only the rule id, a `delegated` flag, the function pointer, the body's predicate accessors, and a wildcard flag. The head and body atoms do not survive into the binary. Yet SPEC-04 says backward chaining is "SPEC-03 + SPEC-07 (using the same compiled rules in a magic-sets context)" and SPEC-07 F4 says the backward mode will "invoke SPEC-03 magic-sets with the OWL 2 RL rule set as the rewrite source." A compiled function cannot be rewritten. The specs assume a form of the rules that nothing produces.

**There is no target.** PLAN-23-06's rewrite passes are unbuilt. Its catalog seam (HDB-211, PR #396) is a read-only trait `ReasoningCatalog` answering `closure_state(pattern) -> {Closed, Partial, NotClosed}` and `cost(pattern, Strategy)` for `Strategy::{Materialize, Rewrite, Delegate(Resolver)}`, with every cost a stub.

**The two plans block each other.** PLAN-23-06 lists PLAN-23-07 as a prerequisite ("supplies the demand-driven machinery that makes the choice real") and PLAN-23-07 lists PLAN-23-06 ("must land first"). No task order satisfies that.

What already exists and can carry the design:

- At build time, `crates/owlrl/codegen/parse.rs` produces `RuleSpec { id, delegate, body: Vec<Pattern>, head: Pattern }` with `Slot::Var(name) | Slot::Vocab(field)`. That is a rule IR. It is thrown away after `emit.rs` has generated the functions.
- At run time, `horndb_wcoj::pattern::{Term::{Bound(TermId), Var}, TriplePattern, Bgp}` already describe a triple pattern and a set of them. A rule body is a BGP; a rule head is one triple pattern.
- `horndb-sparql` already depends on `horndb-wcoj` and, behind the `reasoner` feature, on `horndb-owlrl`. Its `LogicalPlan` has `Bgp`, `Union`, `Distinct`, and `PathClosure` (the recursive property-path node). Its `PassId` registry runs a fixed pass list before `JoinPlanning` (SPEC-23 §5.2).
- 65 `[[rule]]` blocks are in `rules.toml`. Nine carry `delegate = "closure"` (eq-ref, eq-sym, eq-trans, prp-trp, scm-sco, scm-spo, and the three SSSOM transitivity rules) and are computed by the GraphBLAS backend, not compiled. Seven more rules live outside `rules.toml` as hand-written code in `crates/owlrl/src/list_rules.rs` (prp-spo2, prp-key, cls-int1, cls-uni, cax-adc, eq-diff2, eq-diff3) because they walk an `rdf:List` of unknown length.
- The materializer has one mode. `crates/owlrl/src/engine.rs` is "full re-materialization only": `materialize` / `materialize_with` run every rule, and `[reasoning]` is server-scoped with no per-query override (`crates/sparql/src/reasoning/mod.rs`). A store is either fully closed or not reasoned over at all.

The three questions HDB-237 asks: (1) what the runtime rule IR is and which crate owns it; (2) how the compiled rule set reaches a query-time rewriter, or whether magic sets works on a separate rule set; (3) what "feed the result into the PLAN-23-06 passes" means.

## Options considered

**A. Emit a machine-readable rule description from the existing codegen, next to the generated Rust.** `emit.rs` already holds every `RuleSpec`; emitting a second constant table from it is one more emit function. The rewriter reads that table. One source of truth (`rules.toml`) is kept; a rule added tomorrow is rewritable the same day with no extra edit. Cost: a small codegen extension, one new module in `horndb-wcoj`, one conversion function in `horndb-sparql`. Limit: the seven `list_rules.rs` rules have no `rules.toml` entry and so no rewritable form.

**B. A second, hand-maintained query-time rule set.** A Rust constant or a second TOML file listing the rules in rewritable form. Cost: two copies of 65 rules with no compiler to catch a mismatch between them; every rule edit becomes two edits. It contradicts ADR-0004's stated consequence that "`rules.toml` is the single editable source of rule truth." Rejected.

**C. No general rewriter; hard-code the rewrites for a narrow class of rules.** Write the subclass expansion (cax-sco), the subproperty expansion (prp-spo1), and transitive property paths as three passes in `horndb-sparql`. This meets SPEC-03 acceptance criterion 4 (`subClassOf+` over SNOMED CT). Cost: F4 is not delivered as specified; ADR-0005's "backward-chain the remainder" shrinks to "backward-chain three rule shapes"; every further rule needs a new hand-written pass, which is option B by another door. Acceptable as a first slice of A, not as the decision.

**D. Apply the magic-sets transform at build time and compile the adorned rules (Soufflé's approach).** Each predicate has at most eight adornments and the rule set is fixed, so `build.rs` can emit `fire_<id>_<adornment>` variants from the `RuleSpec` it already holds and run them on the existing semi-naive driver over a demand-scoped store. It keeps ADR-0004 untouched, reuses the existing fixpoint engine and its proof recording, and adds no second evaluator. Cost: more generated code, and the sideways-information-passing order is fixed at build time. Limit: the rewrite result is compiled code, so the query optimizer cannot see or cost it, which SPEC-23 §5.8 requires for the non-recursive case.

**Recommendation: A for rewriting, with the recursive stratum's evaluator still to choose.** A gives the optimizer a `Union` of `Bgp`s it can plan. A recursive result becomes a `Fixpoint` node (see 3), which needs an evaluator: either the new semi-naive loop described in 3, or D's adorned compiled rules run by the existing `engine.rs` driver over the demand slice. D avoids a second semi-naive loop beside `engine.rs`; the loop in 3 avoids growing the generated code. This decision picks one. A is staged so that its first shippable unit is the description table plus the rewriter as a pure library, with no change to any shipped query path.

## Decision (proposed)

### 1. The runtime rule IR lives in `horndb-wcoj`

SPEC-03 places F4 in `horndb-wcoj`, and `horndb-sparql` (the only consumer) already depends on it. A new module `horndb_wcoj::rule` defines:

- `Rule { id: &'static str, head: Atom, body: Vec<Atom> }`. Variables are `Var(u8)`, scoped per rule. Constants are `Term::Bound(TermId)` in the query's dictionary.
- `Atom::Triple(TriplePattern) | Atom::Magic { rel: MagicRel, args: Vec<Term> }`. The triple form reuses `pattern::TriplePattern` unchanged. The magic form exists only in rewriter output.
- `Adornment([Binding; 3])`, `Binding::{Bound, Free}`: which of subject, predicate, object of a goal are bound when the goal is called. Written `rdf:type^fb` for `?x rdf:type :C`. The adornment of a goal is read straight off its `Term`s.
- `MagicRel { predicate: TermId, adornment: Adornment }`: the demand relation for one adorned predicate — "which bound-position values has anyone asked for". Its extent is an in-memory set seeded from the query's bound terms. It is never written to the store and never gets a dictionary id.
- `RuleSet`: rules indexed by head predicate, plus the set of predicates every one of whose head rules is closure-delegated (see 2). Rules whose head has a variable predicate are not in the index: `eq-rep-s`, `eq-rep-p` and `eq-rep-o` would unify with every goal and pull `owl:sameAs` into every rewrite. Equality stays on the ADR-0005 materialized `sameAs` closure.
- A goal with an unbound predicate is never rewritten; the `ReasoningCatalog` answers `Strategy::Materialize` for it.
- `magic_rewrite(goal: &TriplePattern, rules: &RuleSet) -> Rewritten { seeds: Vec<(MagicRel, Vec<TermId>)>, rules: Vec<Rule> }`: generalized magic sets (Beeri and Ramakrishnan, "On the power of magic", JLP 1991). Sideways information passing is left to right in the body order written in `rules.toml`. Output is the same `Rule` type, so the result can be printed, tested, and translated without a second data model.

Nothing in this module executes a rule. It is a rule-to-rule transformation with tests that are plain data in, plain data out.

### 2. The rule source is emitted by the existing codegen; there is no second rule set

`crates/owlrl/codegen/emit.rs` additionally emits `pub const RULE_IR: &[RuleDesc]` into `generated_rules.rs`, from the same `Vec<RuleSpec>` the fire functions come from. `RuleDesc` is dictionary-free: `id`, `delegated`, `head`, `body`, with slots `Slot::Var(u8) | Slot::Vocab(PredAccessor)` where `PredAccessor = fn(&Vocabulary) -> TermId` is the accessor type the table already uses for `body_predicates`. A test pins `RULE_IR` against `RULES`: same ids in the same order, and each non-delegated rule's body predicates equal to its `CompiledRule::body_predicates`.

The conversion `RuleDesc -> horndb_wcoj::rule::Rule`, given the `Vocabulary` filled from the query's dictionary, lives in `horndb-sparql` behind the `reasoner` feature. That is the one crate that already sees both sides, so no new crate dependency edge is added. (`owlrl` depending on `wcoj` would also be legal under the workspace order `storage → wcoj → {owlrl, closure}`, but nothing needs it.)

Closure-delegated rules are in `RULE_IR` with `delegated: true`. The rewriter treats their head predicates as base relations: a goal on `rdfs:subClassOf`, `rdfs:subPropertyOf`, `owl:sameAs`, a transitive property, or an SSSOM match reads the store or a closure delegate node, never a rewrite. Those predicates are exactly the subset ADR-0005 materializes, so this is ADR-0005 stated as a rule of the rewriter.

The seven `list_rules.rs` rules are not in `RULE_IR`. A goal that needs one of them cannot be rewritten under this design; the `ReasoningCatalog` must answer `Strategy::Materialize` for it. This is a stated limit, revisited only if a workload hits it. `prp-spo2` (property chains) is the one real ontologies are most likely to hit.

### 2a. The materializer closes a chosen rule subset

The rewrite pass only acts on patterns that are not `Closed`, and today every store is fully closed or not reasoned over (see Context). The hybrid therefore needs a second materialization mode: `materialize` accepts a rule subset and closes only that. The default subset is the closure-delegated rules, the slice ADR-0005 materializes. Everything else is left to demand, and the `ReasoningCatalog`'s `closure_state` reports it as `NotClosed`.

This is also what bounds load-time memory. Full OWL 2 RL materialization peaks at about 3.8 KB per asserted triple, about 450 GiB at SPB SF 0.128, so a full closure is not buildable in one process on a 124 GiB host.

Acceptance: a store closed with the default subset answers the OWL 2 RL-50 subset (`harness/curation/owl2-rl-50.md`) through the rewrite pass with the same results as a fully closed store.

### 3. "Feed into the PLAN-23-06 passes" means: a pass is the rewriter's only caller

The rewriter is a pure function. It is called from one new `PassId` in `horndb-sparql` (working name `ReasoningRewrite`), running before `JoinPlanning`, which for each triple pattern inside a `Bgp`:

1. asks the `ReasoningCatalog` for `closure_state(pattern)`. If `Closed`, does nothing. On a fully materialized store every predicate is closed, so the pass is a no-op and no shipped query changes. It acts only on a store closed with a rule subset (2a);
2. otherwise compares `cost(pattern, Strategy)` for the three strategies. `Delegate` becomes a `PathClosure` / closure-scan node as PLAN-23-06 Task 4 defines; `Materialize` leaves the pattern alone and records that the closure slice must be materialized before execution; `Rewrite` calls `magic_rewrite(pattern, rules)`;
3. splices the translation of the `Rewritten` result in place of the pattern. A non-recursive result becomes a `Union` of `Bgp`s, one per rule body, with the seeds substituted as constants. A recursive result becomes one new logical node, `Fixpoint { seeds, rules }`, executed by a semi-naive loop over the existing executor with a per-execution memo table for the magic and derived relations. That memo table is SPEC-03 F5: results are memoized per query execution, and the loop stops when a round adds nothing, which on a finite store is guaranteed. No separate top-down SLG resolver is built.

The spliced subtree is wrapped so the derived relation is a **set** (`Distinct`, or the fixpoint's own deduplication). A derived relation is a set, so the outer query's bag semantics come out exactly as they would from a materialized store. This answers the double-counting objection recorded on HDB-212.

Under this reading PLAN-23-06 Task 2 (subclass/subproperty rewrite) is not a hand-written expansion: it is the pass applied to goals on `rdf:type` and on sub-properties, with the rewriter doing the expansion from cax-sco and prp-spo1. Task 3 (transitive rule to fixpoint) is the same pass meeting a recursive stratum.

### 4. Ordering the two plans

Once the rewriter is a library, the mutual prerequisite dissolves into a partial order over tasks:

1. PLAN-23-07 Task 1 (`RULE_IR`, the `rule` module, `magic_rewrite`) depends only on this ADR. Subset materialization (2a) also depends only on this ADR; neither plan has a task for it yet.
2. PLAN-23-07 Task 2 (tabling: the `Fixpoint` node's memo table and termination) depends on 1.
3. PLAN-23-06 Tasks 2 and 3 (the passes) depend on 1 and on subset materialization; Task 3 also on 2. Task 4 (delegate nodes) is independent. Task 1 (catalog seam) shipped as a stub.
4. PLAN-23-07 Task 3 (the SPARQL backward-chained mode) depends on PLAN-23-06 Tasks 2–4: the switch needs something to switch on.
5. PLAN-23-06 Task 5 (cost-based choice), then PLAN-23-07 Tasks 4 and 5, then both plans' Task 6.

Neither plan depends on the other as a whole. The plan documents record this order, and the task edges follow it: HDB-212 (PLAN-23-06 Task 2) depends on HDB-217; HDB-213 (Task 3) on HDB-217 and HDB-218; HDB-219 (PLAN-23-07 Task 3) on HDB-212, HDB-213 and HDB-214.

## Consequences

- `+` One source of rule truth. Every `rules.toml` rule, present and future, is rewritable with no extra edit.
- `+` **ADR-0004 is narrowed, not overturned.** Forward materialization still runs only the compiled `fire_<id>` functions; `RULE_IR` is data the materializer never reads. What is new is a query-time evaluator for the demand-restricted rule set. It runs rule bodies as BGPs through the executor that already runs runtime-supplied SPARQL BGPs, so it adds no new kind of per-tuple dispatch; the added cost is the fixpoint loop and the magic relations, and demand bounds both. ADR-0004's "no user-defined runtime rules until the Stage-2 Datalog frontend" still holds: `RULE_IR` is generated, not a user surface. If user rules arrive, they enter through this IR.
- `+` ADR-0005 is preserved exactly: closure-delegated heads are base relations to the rewriter.
- `+` HDB-217 becomes implementable now, with no PLAN-23-06 dependency, and lands with no behaviour change to any shipped query.
- `−` The seven list rules and the three `eq-rep-*` rules are not backward-chainable; a query needing them stays on the materialized path.
- `−` The hybrid needs a second materialization mode (2a). Without it the rewrite pass never fires.
- `−` `Fixpoint` is a new logical operator. Its cost model is SPEC-23 §8 open question 4, which this ADR does not close. First cut: cost it as opaque (the stub value), as PLAN-23-06 Task 3's own guard already says.
- `−` SPEC-03's risk note on magic sets with aggregates and OPTIONAL stands: the pass rewrites patterns inside a `Bgp` only; anything else stays materialized.
- `−` SPEC-03 acceptance criterion 4 (`subClassOf+` over SNOMED CT in at most 2x a materialized scan) needs the `Fixpoint` node and a `hornbench` run. It is not claimable by HDB-217 alone.

## Related

- Realizes: [ADR-0005](0005-hybrid-forward-backward-chaining.md). Narrows: [ADR-0004](0004-compile-owl2rl-rules-ahead-of-time.md) (see Consequences).
- Governing specs: SPEC-03 F4/F5 and acceptance criterion 4; SPEC-04 scope; SPEC-07 F4; SPEC-23 §5.8 and §8 (open question 4).
- Plans: PLAN-23-07 (HDB-PLAN-60) Tasks 1–3; PLAN-23-06 (HDB-PLAN-59) Tasks 2–4. Catalog seam: HDB-211, PR #396.
- Architecture: `docs/architecture.md` §1 (bet 1), the SPEC-03 F4/F5 rows, and the SPEC-07 backward-chained mode row.
- Code the design reuses: `crates/owlrl/codegen/{parse,emit}.rs`, `crates/wcoj/src/pattern.rs`, `crates/sparql/src/plan/{logical,pass}.rs`.
