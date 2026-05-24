use crate::ids::{Ordering, TermId};

/// Variable identifier within a single BGP. Small enough that a `Vec<Var>`
/// of plan-time orderings is cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u8);

/// One slot of a triple pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    Bound(TermId),
    Var(Var),
}

impl Term {
    pub fn as_var(self) -> Option<Var> {
        match self {
            Term::Var(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_bound(self) -> Option<TermId> {
        match self {
            Term::Bound(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TriplePattern {
    pub s: Term,
    pub p: Term,
    pub o: Term,
}

impl TriplePattern {
    pub fn new(s: Term, p: Term, o: Term) -> Self {
        Self { s, p, o }
    }

    pub fn is_ground(&self) -> bool {
        matches!(self.s, Term::Bound(_))
            && matches!(self.p, Term::Bound(_))
            && matches!(self.o, Term::Bound(_))
    }

    /// Heuristic: choose an ordering that puts bound positions at the
    /// shallowest depths. Ties broken `S < P < O`. This is the only
    /// per-pattern ordering decision the Stage-1 planner needs — the
    /// real planner (Stage 2) will jointly optimise across patterns.
    pub fn preferred_ordering(&self) -> Ordering {
        let bound = [
            matches!(self.s, Term::Bound(_)),
            matches!(self.p, Term::Bound(_)),
            matches!(self.o, Term::Bound(_)),
        ];
        // Score each ordering by the depth-weighted sum of "bound at depth d"
        // where shallower (smaller d) is better, so we negate.
        let orderings = [
            (Ordering::Spo, [bound[0], bound[1], bound[2]]),
            (Ordering::Sop, [bound[0], bound[2], bound[1]]),
            (Ordering::Pso, [bound[1], bound[0], bound[2]]),
            (Ordering::Pos, [bound[1], bound[2], bound[0]]),
            (Ordering::Osp, [bound[2], bound[0], bound[1]]),
            (Ordering::Ops, [bound[2], bound[1], bound[0]]),
        ];
        orderings
            .iter()
            .min_by_key(|(_, b)| {
                // Smaller "first non-bound depth" wins; secondary key: order index for stable tiebreak.
                let first_unbound = b.iter().position(|x| !x).unwrap_or(3);
                (3 - first_unbound, 0)
            })
            .map(|(o, _)| *o)
            .unwrap()
    }

    /// Pick a trie ordering consistent with the executor's global variable
    /// elimination order: the resulting physical depth of each variable in
    /// this pattern is monotone non-decreasing in its global var-depth, and
    /// bound positions are pushed as shallow as possible within that
    /// constraint. Falls back to `preferred_ordering()` if no consistent
    /// ordering exists (impossible for triple patterns with ≤2 vars).
    pub fn ordering_for(&self, var_order: &[Var]) -> Ordering {
        use Ordering::*;
        let all = [Spo, Sop, Pso, Pos, Osp, Ops];
        let phys_of = |ord: Ordering| -> [Term; 3] {
            match ord {
                Spo => [self.s, self.p, self.o],
                Sop => [self.s, self.o, self.p],
                Pso => [self.p, self.s, self.o],
                Pos => [self.p, self.o, self.s],
                Osp => [self.o, self.s, self.p],
                Ops => [self.o, self.p, self.s],
            }
        };
        // Score: (consistent?, bound-depth-sum). Consistent=true wins;
        // smaller bound-depth-sum wins as tiebreak (mirrors preferred_ordering).
        let mut best: Option<(Ordering, (bool, isize, isize))> = None;
        for &ord in &all {
            let phys = phys_of(ord);
            // Compute phys-depth of each pattern var in global var-depth order.
            let mut prev_phys: i32 = -1;
            let mut consistent = true;
            for v in var_order {
                for (pd, t) in phys.iter().enumerate() {
                    if let Term::Var(vv) = t {
                        if vv == v {
                            let p = pd as i32;
                            if p < prev_phys {
                                consistent = false;
                            }
                            prev_phys = p;
                            break;
                        }
                    }
                }
            }
            // Bound-depth-sum: smaller = bounds at shallower depths.
            let bound_sum: isize = phys
                .iter()
                .enumerate()
                .map(|(d, t)| if matches!(t, Term::Bound(_)) { d as isize } else { 0 })
                .sum();
            // Stable index for tiebreaks.
            let idx = all.iter().position(|&o| o == ord).unwrap() as isize;
            // Minimise (!consistent, bound_sum, idx). For tiebreak `idx`,
            // smaller index wins (matches array order Spo < Sop < ...).
            let score = (!consistent, bound_sum, idx);
            if best.map(|(_, b)| score < b).unwrap_or(true) {
                best = Some((ord, score));
            }
        }
        best.map(|(o, _)| o).unwrap_or_else(|| self.preferred_ordering())
    }

    /// Return the position (0=S, 1=P, 2=O) of the given variable, or `None`.
    pub fn position_of(&self, v: Var) -> Option<u8> {
        if self.s == Term::Var(v) {
            Some(0)
        } else if self.p == Term::Var(v) {
            Some(1)
        } else if self.o == Term::Var(v) {
            Some(2)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bgp {
    pub patterns: Vec<TriplePattern>,
}

impl Bgp {
    pub fn new(patterns: Vec<TriplePattern>) -> Self {
        Self { patterns }
    }

    /// All variables appearing in any pattern, in first-appearance order.
    pub fn variables(&self) -> Vec<Var> {
        let mut out = Vec::new();
        for p in &self.patterns {
            for t in [p.s, p.p, p.o] {
                if let Term::Var(v) = t {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }
}
