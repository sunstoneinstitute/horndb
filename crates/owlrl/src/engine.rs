//! Semi-naïve evaluation driver. Stage 1: full re-materialization only.

use crate::backend::ClosureBackend;
use crate::delta::Delta;
use crate::eq_rep_p_opt::fire_eq_rep_p_canonical;
use crate::generated::{CompiledRule, RULES};
use crate::list_rules::{self, SchemaAxioms};
use crate::store::TripleStore;
use crate::types::TermId;
use crate::vocab::Vocabulary;
use rustc_hash::FxHashSet;

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub rounds: usize,
    pub triples_inferred: usize,
    pub rule_fires: usize,
    /// Wall-clock attribution across the materialize phases. Collected
    /// unconditionally — the `Instant` overhead is a few nanoseconds per round
    /// and negligible against any real workload. Used by `crates/bench-rdfox`
    /// to attribute the LUBM materialize cost (#61).
    pub timings: PhaseTimings,
}

/// Per-phase wall-clock totals summed across every semi-naïve round.
#[derive(Debug, Default, Clone)]
pub struct PhaseTimings {
    /// Time in the compiled `rules.toml` fire functions (incl. `eq-rep-p`).
    pub compiled_rules: std::time::Duration,
    /// Time in the hand-written list-axiom rules (`list_rules::fire_all`).
    pub list_rules: std::time::Duration,
    /// Time in `ClosureBackend::close` (the transitive-closure rules).
    pub closure_backend: std::time::Duration,
    /// Time applying the round delta to the store (`insert_inferred`).
    pub apply: std::time::Duration,
}

/// How the engine evaluates the `eq-rep-p` rule.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum EqRepPStrategy {
    /// Predicate-equivalence-class canonicalization (default). Identical
    /// closure to `Naive`, bounded work — see `eq_rep_p_opt`.
    #[default]
    Optimized,
    /// The generated `fire_eq_rep_p` nested-loop firing. Retained as the
    /// correctness oracle for `tests/eq_rep_p_differential.rs`.
    Naive,
}

/// How the engine evaluates the `rdf:type`-driven list rules (`cls-int1`,
/// `cls-uni`, `cax-adc`, `prp-key`) — SPEC-04 F5.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ParallelStrategy {
    /// Partition `rdf:type` work by class id and parallelise the per-subject
    /// filtering across rayon's pool above a tuned subject-count threshold
    /// (default). Identical closure to `Serial` — see
    /// `tests/rdf_type_skew_differential.rs`.
    #[default]
    Auto,
    /// Force the original sequential per-subject scan. Retained as the
    /// correctness oracle for the F5 differential test and for callers that
    /// want deterministic single-threaded execution.
    Serial,
}

/// Tunables for a `materialize` run.
#[derive(Copy, Clone, Debug, Default)]
pub struct MaterializeOpts {
    pub eq_rep_p: EqRepPStrategy,
    /// `rdf:type`-skew parallelism for the list rules (SPEC-04 F5).
    pub parallel: ParallelStrategy,
}

/// Run forward chaining to fixed point. Does NOT clear existing inferred
/// triples — see `reset_and_materialize` for that.
pub fn materialize<S: TripleStore + Sync, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    materialize_with(store, backend, MaterializeOpts::default())
}

/// As `materialize`, with explicit strategy selection.
///
/// `S: Sync` is required by the SPEC-04 F5 parallel list-rule path, which shares
/// `&store` across rayon worker threads. The only `TripleStore` impl (`MemStore`)
/// is `Sync`, so this bound is invisible to callers.
pub fn materialize_with<S: TripleStore + Sync, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
    opts: MaterializeOpts,
) -> Stats {
    let mut stats = Stats::default();
    // Resolve list-axiom schemas once per materialize pass. Stage-1 is
    // insertion-only (SPEC-06) so the resolved chains do not change as
    // round-deltas accumulate — they are entirely schema-shaped.
    let vocab = *store.vocab();
    let axioms = list_rules::resolve(store_as_dyn(store), &vocab);
    // First round: every rule fires; treat all predicates as "dirty".
    let mut dirty: Option<FxHashSet<TermId>> = None;
    loop {
        stats.rounds += 1;
        let mut round_delta = Delta::new();

        // 1. Compiled rules.
        let t_compiled = std::time::Instant::now();
        for rule in RULES {
            if rule.delegated {
                continue;
            }
            // Count every non-delegated rule evaluation (prune denominator).
            horndb_metrics::metrics().owlrl.rule_considered.inc();
            // eq-rep-p is special-cased: under the Optimized strategy the
            // engine substitutes a class-canonical pass (eq_rep_p_opt) for
            // the generated nested-loop fire — identical closure, bounded
            // work (TASKS.md #2 / SPEC-04 F5). The id-string match is the
            // single deliberate coupling between the driver and a rule.
            if rule.id == "eq-rep-p" && opts.eq_rep_p == EqRepPStrategy::Optimized {
                if rule_relevant(rule, dirty.as_ref(), store.vocab()) {
                    stats.rule_fires += 1;
                    let label = horndb_metrics::labels::RuleLabel {
                        rule: rule.id.to_string(),
                    };
                    horndb_metrics::metrics()
                        .owlrl
                        .rule_fires
                        .get_or_create(&label)
                        .inc();
                    let t_rule = std::time::Instant::now();
                    round_delta.merge(fire_eq_rep_p_canonical(store_as_dyn(store)));
                    horndb_metrics::metrics()
                        .owlrl
                        .rule_duration_seconds
                        .get_or_create(&label)
                        .observe(t_rule.elapsed().as_secs_f64());
                } else {
                    horndb_metrics::metrics().owlrl.rule_pruned.inc();
                }
                continue;
            }
            if !rule_relevant(rule, dirty.as_ref(), store.vocab()) {
                horndb_metrics::metrics().owlrl.rule_pruned.inc();
                continue;
            }
            stats.rule_fires += 1;
            let label = horndb_metrics::labels::RuleLabel {
                rule: rule.id.to_string(),
            };
            horndb_metrics::metrics()
                .owlrl
                .rule_fires
                .get_or_create(&label)
                .inc();
            let t_rule = std::time::Instant::now();
            let d = (rule.fire)(store_as_dyn(store), &Delta::new());
            horndb_metrics::metrics()
                .owlrl
                .rule_duration_seconds
                .get_or_create(&label)
                .observe(t_rule.elapsed().as_secs_f64());
            round_delta.merge(d);
        }
        stats.timings.compiled_rules += t_compiled.elapsed();

        // 2. Hand-written list-axiom rules (prp-spo2, prp-key, cls-int1,
        //    cls-uni, cax-adc, eq-diff2/3). See `list_rules.rs` for why
        //    these live outside `rules.toml`.
        let t_list = std::time::Instant::now();
        if list_rules_relevant(&axioms, dirty.as_ref(), &vocab) {
            let d = list_rules::fire_all(
                store_as_dyn_sync(store),
                &axioms,
                &vocab,
                dirty.as_ref(),
                opts.parallel,
            );
            round_delta.merge(d);
        }
        stats.timings.list_rules += t_list.elapsed();

        // 3. Closure backend (handles delegated rules).
        let t_closure = std::time::Instant::now();
        let backend_delta = backend.close(store_as_dyn(store));
        round_delta.merge(backend_delta);
        stats.timings.closure_backend += t_closure.elapsed();

        // 4. Apply to store.
        let t_apply = std::time::Instant::now();
        let mut new_count = 0;
        let mut applied = Delta::new();
        for (t, prov) in round_delta.iter() {
            if store.insert_inferred(*t, prov.clone()) {
                new_count += 1;
                applied.insert(*t, prov.clone());
            }
        }
        stats.triples_inferred += new_count;
        stats.timings.apply += t_apply.elapsed();

        if applied.is_empty() {
            break;
        }
        dirty = Some(applied.dirty_predicates());
    }
    // Emit aggregate counters and per-phase histograms once per
    // materialize_with call, after the loop converges.
    {
        let m = horndb_metrics::metrics();
        m.owlrl
            .triples_inferred
            .inc_by(stats.triples_inferred as u64);
        m.owlrl.rounds.inc_by(stats.rounds as u64);
        use horndb_metrics::labels::{Phase, PhaseLabel};
        for (phase, dur) in [
            (Phase::CompiledRules, stats.timings.compiled_rules),
            (Phase::ListRules, stats.timings.list_rules),
            (Phase::ClosureBackend, stats.timings.closure_backend),
            (Phase::Apply, stats.timings.apply),
        ] {
            m.owlrl
                .phase_duration_seconds
                .get_or_create(&PhaseLabel { phase })
                .observe(dur.as_secs_f64());
        }
    }
    stats
}

fn list_rules_relevant(
    axioms: &SchemaAxioms,
    dirty: Option<&FxHashSet<TermId>>,
    vocab: &Vocabulary,
) -> bool {
    if axioms.is_empty() {
        return false;
    }
    let Some(dirty) = dirty else {
        return true;
    };
    let body_preds = axioms.body_predicates(vocab);
    body_preds.iter().any(|p| dirty.contains(p))
}

/// Drop all inferred triples and re-run forward chaining from the asserted base.
/// Implements SPEC-04 F7.
pub fn reset_and_materialize<S: TripleStore + Sync, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    store.clear_inferred();
    materialize(store, backend)
}

fn rule_relevant(
    rule: &CompiledRule,
    dirty: Option<&FxHashSet<TermId>>,
    vocab: &Vocabulary,
) -> bool {
    // First round (dirty = None): everything is relevant.
    let Some(dirty) = dirty else { return true };
    // A rule whose body contains a variable-predicate pattern (e.g.
    // `?s ?p ?o` in eq-rep-s/p/o) reads triples with any predicate, so any
    // dirty predicate in the previous round could expose a new match. Such
    // rules cannot participate in the dirty-predicate prune — they must
    // re-fire on every subsequent round.
    if rule.wildcard_predicate {
        return true;
    }
    rule.body_predicates
        .iter()
        .any(|pa| dirty.contains(&pa(vocab)))
}

/// Coerce a generic `&S` to `&dyn TripleStore`. Needed because `RULES`
/// entries take `&dyn TripleStore`.
fn store_as_dyn<S: TripleStore>(s: &S) -> &dyn TripleStore {
    s
}

/// Coerce a generic `&S` to `&(dyn TripleStore + Sync)`. The SPEC-04 F5
/// list-rule path (`list_rules::fire_all`) shares this reference across rayon
/// threads, so it needs the `Sync` bound the plain `store_as_dyn` drops.
fn store_as_dyn_sync<S: TripleStore + Sync>(s: &S) -> &(dyn TripleStore + Sync) {
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::RuleFiringBackend;
    use crate::store::MemStore;
    use crate::types::{TermId, Triple};
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn empty_store_terminates() {
        let v = Vocabulary::synthetic(1000);
        let mut store = MemStore::new(v);
        let mut backend = RuleFiringBackend::new();
        let stats = materialize(&mut store, &mut backend);
        assert_eq!(stats.triples_inferred, 0);
        assert!(stats.rounds >= 1);
    }

    #[test]
    fn cax_sco_two_hop() {
        let v = Vocabulary::synthetic(1000);
        let (sco, ty) = (v.rdfs_sub_class_of, v.rdf_type);
        let (a, b, c, x) = (TermId(1), TermId(2), TermId(3), TermId(4));
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C, x : A
        store.assert(t(a.0, sco.0, b.0));
        store.assert(t(b.0, sco.0, c.0));
        store.assert(t(x.0, ty.0, a.0));
        let mut backend = RuleFiringBackend::new();
        materialize(&mut store, &mut backend);
        assert!(
            store.contains(&t(x.0, ty.0, b.0)),
            "expected x : B (cax-sco)"
        );
        assert!(
            store.contains(&t(x.0, ty.0, c.0)),
            "expected x : C (cax-sco + scm-sco)"
        );
    }
}
