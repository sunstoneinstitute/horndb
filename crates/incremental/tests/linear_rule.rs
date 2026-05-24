//! `LinearRule` example: a synthetic `scm-*`-shaped rule that rewrites
//! triples of the form `(s, P, o)` into `(s, P', o)`. Linear in its
//! input — the delta passes straight through with the rule applied.

use reasoner_incremental::{LinearRule, RuleId, TripleId, Zset};

struct RewritePredicate {
    id: RuleId,
    from: u64,
    to: u64,
}

impl LinearRule for RewritePredicate {
    fn id(&self) -> RuleId {
        self.id
    }

    fn apply_delta(&self, delta: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((s, p, o), m) in delta.iter() {
            if *p == self.from {
                out.add((*s, self.to, *o), m);
            }
        }
        out
    }
}

#[test]
fn linear_rule_passes_delta_through_with_rewrite() {
    let rule = RewritePredicate {
        id: 1,
        from: 100,
        to: 200,
    };
    let delta = Zset::from_iter([((1, 100, 2), 1), ((3, 100, 4), 1), ((5, 999, 6), 1)]);

    let out = rule.apply_delta(&delta);
    assert_eq!(out.get(&(1, 200, 2)), 1);
    assert_eq!(out.get(&(3, 200, 4)), 1);
    assert_eq!(out.get(&(5, 999, 6)), 0);
    assert_eq!(out.len(), 2);
}

#[test]
fn linearity_delta_of_union_is_union_of_deltas() {
    // Linearity: f(a + b) == f(a) + f(b)
    let rule = RewritePredicate {
        id: 1,
        from: 100,
        to: 200,
    };
    let a = Zset::from_iter([((1, 100, 2), 1)]);
    let b = Zset::from_iter([((3, 100, 4), 1)]);

    let mut ab = a.clone();
    ab.add_assign(&b);
    let f_ab = rule.apply_delta(&ab);

    let mut sum = rule.apply_delta(&a);
    sum.add_assign(&rule.apply_delta(&b));

    assert_eq!(f_ab, sum);
}
