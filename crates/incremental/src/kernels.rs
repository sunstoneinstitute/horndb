//! `HashJoinRule` — a generic hash-join `BilinearRule` kernel (SPEC-24
//! §S7 leaf 2).
//!
//! Every hand-written `BilinearRule` fixture in this crate today is an
//! O(|a|·|b|) nested loop, one per rule shape. This is the one runtime a
//! rule author (or SPEC-04/E4's codegen, [#188]) instantiates instead of
//! writing a new nested loop: it is parameterised by two body triple
//! patterns and a head pattern, and is correct for arbitrary Z-set
//! multiplicities. Choosing the join order, slicing inputs by predicate,
//! and codegen that emits instances of this kernel from `rules.toml` are
//! later S7 leaves — see `docs/specs/SPEC-24-incremental-stage2.md`.
//!
//! [#188]: https://github.com/sunstoneinstitute/horndb/issues/188

use std::collections::HashMap;

use horndb_wcoj::{Term, TriplePattern, Var};
use thiserror::Error;

use crate::operator::BilinearRule;
use crate::types::{Multiplicity, RuleId, TripleId};
use crate::zset::Zset;

/// Largest `Var(u8)` index this kernel's binding table can hold. Rule
/// shapes here (and the ones SPEC-04 codegen is expected to emit for a
/// single `NaryPlan` leaf) use a handful of variables; a shape needing
/// more can raise this constant.
const MAX_VARS: usize = 8;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    #[error("head variable {0:?} does not occur in either body pattern")]
    UnboundHeadVar(Var),
    #[error("left and right body patterns share no variable — would be a cross product")]
    NoSharedVar,
    #[error("n-ary planning needs at least two body patterns, got {0}")]
    BodyTooShort(usize),
    #[error("body is disconnected — no unused pattern shares a variable with the join prefix")]
    DisconnectedBody,
    #[error(
        "join prefix has {0} live variables; a triple-shaped intermediate holds at most three"
    )]
    TooManyLiveVars(usize),
}

/// A generic two-pattern hash join, correct for arbitrary Z-set
/// multiplicities (SPEC-24 §S7 leaf 2). `left`/`right` are the two body
/// triple patterns, `head` is the pattern the join result is bound into.
#[derive(Debug)]
pub struct HashJoinRule {
    id: RuleId,
    left: TriplePattern,
    right: TriplePattern,
    head: TriplePattern,
}

/// Binds each `Var` in a triple pattern to the value it saw in one row.
/// Not a general evaluator — just a fixed-size lookup table keyed by
/// `Var.0`, sized for the handful of variables one join sees.
#[derive(Clone, Copy, Default)]
struct Bindings([Option<u64>; MAX_VARS]);

impl Bindings {
    fn get(&self, v: Var) -> Option<u64> {
        self.0[v.0 as usize]
    }

    /// Binds `v` to `val`. Returns `false` if `v` was already bound to a
    /// different value — the repeated-variable-in-one-pattern case (e.g.
    /// `(?x p ?x)`), honoured as an equality filter.
    fn set(&mut self, v: Var, val: u64) -> bool {
        match self.0[v.0 as usize] {
            Some(existing) => existing == val,
            None => {
                self.0[v.0 as usize] = Some(val);
                true
            }
        }
    }

    /// Combine two binding tables built from the two sides of a join.
    /// Shared variables carry the same value on both sides by
    /// construction (the join key matched them), so precedence between
    /// the two doesn't matter.
    fn merge(&self, other: &Bindings) -> Bindings {
        let mut out = *self;
        for i in 0..MAX_VARS {
            if let Some(v) = other.0[i] {
                out.0[i] = Some(v);
            }
        }
        out
    }
}

/// Distinct variables in a pattern, first-appearance order (S, P, O).
pub(crate) fn vars_in(pattern: &TriplePattern) -> Vec<Var> {
    let mut vars = Vec::new();
    for t in [pattern.s, pattern.p, pattern.o] {
        if let Term::Var(v) = t {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
    }
    vars
}

/// Variables that occur in both patterns — the join key. At most 3 (a
/// pattern has only 3 slots), matching the `[u64; 3]` key below.
fn shared_vars(left: &TriplePattern, right: &TriplePattern) -> Vec<Var> {
    let right_vars = vars_in(right);
    vars_in(left)
        .into_iter()
        .filter(|v| right_vars.contains(v))
        .collect()
}

/// Matches `triple` against `pattern`: `Bound` terms must equal the
/// corresponding component, and a `Var` repeated within the pattern must
/// see the same value at every occurrence. Returns the resulting
/// bindings, or `None` if the row doesn't match.
fn bind_row(pattern: &TriplePattern, triple: &TripleId) -> Option<Bindings> {
    let mut bindings = Bindings::default();
    let (s, p, o) = *triple;
    for (term, val) in [(pattern.s, s), (pattern.p, p), (pattern.o, o)] {
        match term {
            Term::Bound(bound) => {
                if bound != val {
                    return None;
                }
            }
            Term::Var(v) => {
                if !bindings.set(v, val) {
                    return None;
                }
            }
        }
    }
    Some(bindings)
}

fn filter_and_bind(
    pattern: &TriplePattern,
    input: &Zset<TripleId>,
) -> Vec<(Bindings, Multiplicity)> {
    input
        .iter()
        .filter_map(|(t, m)| bind_row(pattern, t).map(|b| (b, m)))
        .collect()
}

/// The join key for `bindings`: the shared variables' bound values,
/// unused slots (fewer than 3 shared variables) padded with 0.
fn key_of(bindings: &Bindings, shared: &[Var]) -> [u64; 3] {
    let mut key = [0u64; 3];
    for (i, v) in shared.iter().enumerate() {
        key[i] = bindings
            .get(*v)
            .expect("shared var bound by its own pattern");
    }
    key
}

fn eval_head(head: &TriplePattern, bindings: &Bindings) -> TripleId {
    let eval = |t: Term| match t {
        Term::Bound(v) => v,
        Term::Var(v) => bindings
            .get(v)
            .expect("HashJoinRule::new validated every head var is bound by the body"),
    };
    (eval(head.s), eval(head.p), eval(head.o))
}

impl HashJoinRule {
    /// ponytail: `NaryPlan` threads intermediates as `Zset<TripleId>`, so
    /// a join's head — including an intermediate level's head — can carry
    /// at most three bound variables. A body whose prefix needs a fourth
    /// live variable can't be expressed as a left-deep `NaryPlan` today.
    /// Upgrade path: a wider intermediate key type on `BilinearRule` /
    /// `NaryPlan` — E4 territory.
    pub fn new(
        id: RuleId,
        left: TriplePattern,
        right: TriplePattern,
        head: TriplePattern,
    ) -> Result<Self, KernelError> {
        let body_vars = vars_in(&left)
            .into_iter()
            .chain(vars_in(&right))
            .collect::<Vec<_>>();
        for t in [head.s, head.p, head.o] {
            if let Term::Var(v) = t {
                if !body_vars.contains(&v) {
                    return Err(KernelError::UnboundHeadVar(v));
                }
            }
        }
        if shared_vars(&left, &right).is_empty() {
            return Err(KernelError::NoSharedVar);
        }
        Ok(Self {
            id,
            left,
            right,
            head,
        })
    }

    /// Filters and binds each side against its pattern, hash-joins on the
    /// shared variables (building on the smaller filtered side), and
    /// binds `head` for every matching pair.
    fn join(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let left_rows = filter_and_bind(&self.left, a);
        let right_rows = filter_and_bind(&self.right, b);
        let mut out = Zset::new();
        if left_rows.is_empty() || right_rows.is_empty() {
            return out;
        }
        let shared = shared_vars(&self.left, &self.right);
        let (build, probe, build_is_left) = if left_rows.len() <= right_rows.len() {
            (&left_rows, &right_rows, true)
        } else {
            (&right_rows, &left_rows, false)
        };

        let mut index: HashMap<[u64; 3], Vec<(Bindings, Multiplicity)>> = HashMap::new();
        for (bindings, m) in build {
            index
                .entry(key_of(bindings, &shared))
                .or_default()
                .push((*bindings, *m));
        }

        for (probe_bindings, pm) in probe {
            let Some(matches) = index.get(&key_of(probe_bindings, &shared)) else {
                continue;
            };
            for (build_bindings, bm) in matches {
                let (left_b, right_b, ma, mb) = if build_is_left {
                    (build_bindings, probe_bindings, *bm, *pm)
                } else {
                    (probe_bindings, build_bindings, *pm, *bm)
                };
                let combined = left_b.merge(right_b);
                out.add(eval_head(&self.head, &combined), ma * mb);
            }
        }
        out
    }
}

impl BilinearRule for HashJoinRule {
    fn id(&self) -> RuleId {
        self.id
    }

    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        self.join(a, b)
    }

    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        let mut out = self.join(da, b);
        out.add_assign(&self.join(a, db));
        out.add_assign(&self.join(da, db));
        out
    }

    fn body_predicates(&self) -> [Option<u64>; 2] {
        [self.left.p.as_bound(), self.right.p.as_bound()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(n: u8) -> Term {
        Term::Var(Var(n))
    }
    fn bound(v: u64) -> Term {
        Term::Bound(v)
    }
    fn pat(s: Term, p: Term, o: Term) -> TriplePattern {
        TriplePattern::new(s, p, o)
    }

    const P: u64 = 7;
    const Q: u64 = 8;
    const R: u64 = 9;

    /// `(?x P ?y),(?y P ?z) -> (?x P ?z)`: the transitive self-join shape,
    /// valid construction.
    fn transitive_shape() -> Result<HashJoinRule, KernelError> {
        HashJoinRule::new(
            1,
            pat(var(0), bound(P), var(1)),
            pat(var(1), bound(P), var(2)),
            pat(var(0), bound(P), var(2)),
        )
    }

    #[test]
    fn valid_shape_constructs() {
        assert!(transitive_shape().is_ok());
    }

    #[test]
    fn head_var_not_in_body_is_rejected() {
        // Head uses ?w (Var(3)), which neither body pattern binds.
        let err = HashJoinRule::new(
            1,
            pat(var(0), bound(P), var(1)),
            pat(var(1), bound(Q), var(2)),
            pat(var(0), bound(R), var(3)),
        )
        .unwrap_err();
        assert_eq!(err, KernelError::UnboundHeadVar(Var(3)));
    }

    #[test]
    fn disjoint_patterns_are_rejected_as_cross_product() {
        // Left and right share no variable.
        let err = HashJoinRule::new(
            1,
            pat(var(0), bound(P), var(1)),
            pat(var(2), bound(Q), var(3)),
            pat(var(0), bound(R), var(2)),
        )
        .unwrap_err();
        assert_eq!(err, KernelError::NoSharedVar);
    }

    /// The third `new()` validation isn't a rejection: a variable repeated
    /// within one pattern (`(?x p ?x)`) is accepted and honoured as an
    /// equality filter at apply time.
    #[test]
    fn repeated_var_in_one_pattern_constructs_and_filters() {
        let rule = HashJoinRule::new(
            1,
            pat(var(0), bound(P), var(0)), // (?x P ?x) — s and o must match
            pat(var(0), bound(Q), var(1)),
            pat(var(1), bound(R), var(1)),
        )
        .unwrap();

        let a = Zset::from_iter([((1, P, 1), 1), ((2, P, 3), 1)]);
        let b = Zset::from_iter([((1, Q, 5), 1), ((2, Q, 6), 1)]);
        let out = rule.apply_full(&a, &b);
        // Only (1,P,1) satisfies the ?x==?x filter, joining with (1,Q,5).
        assert_eq!(out.get(&(5, R, 5)), 1);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn body_predicates_reports_bound_predicates() {
        let rule = transitive_shape().unwrap();
        assert_eq!(rule.body_predicates(), [Some(P), Some(P)]);
    }

    #[test]
    fn apply_full_matches_nested_loop_reference() {
        let rule = transitive_shape().unwrap();
        let a = Zset::from_iter([((0, P, 1), 1), ((1, P, 2), 2)]);
        let b = Zset::from_iter([((1, P, 2), 1), ((2, P, 3), -1)]);
        let out = rule.apply_full(&a, &b);
        // (0,P,1)x(1,P,2): 1*1=1 -> (0,P,2)
        // (1,P,2)x(2,P,3): 2*-1=-2 -> (1,P,3)
        assert_eq!(out.get(&(0, P, 2)), 1);
        assert_eq!(out.get(&(1, P, 3)), -2);
    }
}
