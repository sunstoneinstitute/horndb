//! Hash-join tree executor over a [`JoinSpec`].
//!
//! Two jobs: (1) run the hybrid plans the cost-based planner emits — hash
//! joins between scans and WCOJ sub-joins; (2) with a left-deep spec of
//! plain scans (`BinaryHashExecutor::new`), serve as the reference
//! implementation for the differential fuzzer (SPEC-03 acceptance #3).
//!
//! Every node materialises its rows eagerly: `Scan` walks the pattern's
//! preferred ordering and filters on bound positions, `Wcoj` drains the
//! leapfrog executor over the sub-BGP, `HashJoin` builds on one child and
//! probes with the other (an empty build side short-circuits the probe).

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, UInt64Array};
use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::pattern::{Bgp, Term, TriplePattern, Var};
use crate::plan::{ExecutionPlan, JoinSpec, PlanKind};
use crate::source::{OrderedTripleIter, TripleSource};

pub struct BinaryHashExecutor<'src, S: TripleSource + ?Sized + 'src> {
    source: &'src S,
    bgp: Arc<Bgp>,
    spec: JoinSpec,
    out_vars: Vec<Var>,
    cancel: CancelToken,
}

impl<'src, S: TripleSource + ?Sized + 'src> BinaryHashExecutor<'src, S> {
    /// Reference oracle: left-deep hash joins of plain scans in pattern
    /// order. Never touches the leapfrog executor.
    pub fn new(source: &'src S, bgp: &Bgp, out_vars: Vec<Var>, cancel: CancelToken) -> Self {
        let spec = JoinSpec::left_deep(0..bgp.patterns.len()).unwrap_or(JoinSpec::Wcoj {
            patterns: Vec::new(),
            var_order: Vec::new(),
        });
        Self::for_spec(source, bgp, spec, out_vars, cancel)
    }

    pub fn for_spec(
        source: &'src S,
        bgp: &Bgp,
        spec: JoinSpec,
        out_vars: Vec<Var>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            source,
            bgp: Arc::new(bgp.clone()),
            spec,
            out_vars,
            cancel,
        }
    }

    // Intentionally named `into_iter` to match the `WcojExecutor` shape;
    // we do not impl `IntoIterator` because the trait would force the
    // returned `BatchIter` to be the executor's only `IntoIter` form and
    // we want callers to spell the conversion explicitly.
    #[allow(clippy::should_implement_trait)]
    pub fn into_iter(self) -> BatchIter<'src, S> {
        BatchIter::new(self)
    }

    /// Materialise one node: output variables and rows in that order.
    fn eval(&self, spec: &JoinSpec) -> Result<(Vec<Var>, Vec<Vec<TermId>>)> {
        self.cancel.check()?;
        match spec {
            JoinSpec::Scan { pattern } => {
                let pat = &self.bgp.patterns[*pattern];
                let vars = crate::cost::pattern_vars(pat);
                let rows = scan_pattern(self.source, pat)?
                    .into_iter()
                    .map(|t| project(pat, t, &vars))
                    .collect();
                Ok((vars, rows))
            }
            JoinSpec::Wcoj {
                patterns,
                var_order,
            } => {
                if patterns.is_empty() {
                    // The join identity: one row binding nothing.
                    return Ok((Vec::new(), vec![Vec::new()]));
                }
                let sub = Bgp::new(patterns.iter().map(|&i| self.bgp.patterns[i]).collect());
                let plan = ExecutionPlan {
                    kind: PlanKind::Wcoj,
                    var_order: var_order.clone(),
                };
                let mut rows = Vec::new();
                for batch in
                    super::wcoj::WcojExecutor::new(self.source, &sub, &plan, self.cancel.clone())
                        .into_iter()
                {
                    let batch = batch?;
                    let cols: Vec<&UInt64Array> = (0..batch.num_columns())
                        .map(|i| {
                            batch
                                .column(i)
                                .as_any()
                                .downcast_ref::<UInt64Array>()
                                .expect("binding columns are u64")
                        })
                        .collect();
                    for r in 0..batch.num_rows() {
                        rows.push(cols.iter().map(|c| c.value(r)).collect());
                    }
                }
                Ok((var_order.clone(), rows))
            }
            JoinSpec::HashJoin { build, probe } => {
                let (bvars, brows) = self.eval(build)?;
                let (pvars, prows) = if brows.is_empty() {
                    (probe.vars(&self.bgp), Vec::new())
                } else {
                    self.eval(probe)?
                };
                Ok(hash_join(&bvars, &brows, &pvars, &prows))
            }
        }
    }
}

/// Hash join on the shared variables; no shared variable is a cross product.
/// Output columns: build's variables, then probe's new ones.
fn hash_join(
    bvars: &[Var],
    brows: &[Vec<TermId>],
    pvars: &[Var],
    prows: &[Vec<TermId>],
) -> (Vec<Var>, Vec<Vec<TermId>>) {
    let keys: Vec<Var> = bvars
        .iter()
        .filter(|v| pvars.contains(v))
        .copied()
        .collect();
    let bkey: Vec<usize> = keys
        .iter()
        .map(|v| bvars.iter().position(|x| x == v).unwrap())
        .collect();
    let pkey: Vec<usize> = keys
        .iter()
        .map(|v| pvars.iter().position(|x| x == v).unwrap())
        .collect();
    let mut out_vars = bvars.to_vec();
    let mut pextra: Vec<usize> = Vec::new();
    for (i, v) in pvars.iter().enumerate() {
        if !bvars.contains(v) {
            out_vars.push(*v);
            pextra.push(i);
        }
    }
    let mut ht: HashMap<Vec<TermId>, Vec<&Vec<TermId>>> = HashMap::new();
    for br in brows {
        ht.entry(bkey.iter().map(|&i| br[i]).collect())
            .or_default()
            .push(br);
    }
    let mut out = Vec::new();
    for pr in prows {
        let key: Vec<TermId> = pkey.iter().map(|&i| pr[i]).collect();
        if let Some(matches) = ht.get(&key) {
            for br in matches {
                let mut row = (*br).clone();
                row.extend(pextra.iter().map(|&i| pr[i]));
                out.push(row);
            }
        }
    }
    (out_vars, out)
}

/// All matching triples for a single pattern, walking the ordering that
/// puts the pattern's bound positions first so seeks skip most of the trie.
fn scan_pattern<S: TripleSource + ?Sized>(source: &S, pat: &TriplePattern) -> Result<Vec<Triple>> {
    let ord = pat.preferred_ordering();
    match scan_ordered(source, pat, ord) {
        Ok(v) => Ok(v),
        // Source cannot serve that ordering: every source serves `Spo`.
        Err(WcojError::OrderingUnavailable(_)) if ord != Ordering::Spo => {
            scan_ordered(source, pat, Ordering::Spo)
        }
        Err(e) => Err(e),
    }
}

fn scan_ordered<S: TripleSource + ?Sized>(
    source: &S,
    pat: &TriplePattern,
    ord: Ordering,
) -> Result<Vec<Triple>> {
    let req = ord.permute(pat.s, pat.p, pat.o);
    let want = |lvl: usize| match req[lvl] {
        Term::Bound(id) => Some(id),
        Term::Var(_) => None,
    };
    let mut iter = source.iter(ord)?;
    let mut out = Vec::new();

    while let Some(l0) = iter.peek(0) {
        if let Some(r) = want(0) {
            if l0 < r {
                iter.seek(0, r);
                continue;
            }
            if l0 > r {
                break;
            }
        }
        iter.open_level(1);
        while let Some(l1) = iter.peek(1) {
            if let Some(r) = want(1) {
                if l1 < r {
                    iter.seek(1, r);
                    continue;
                }
                if l1 > r {
                    break;
                }
            }
            iter.open_level(2);
            while let Some(l2) = iter.peek(2) {
                if let Some(r) = want(2) {
                    if l2 < r {
                        iter.seek(2, r);
                        continue;
                    }
                    if l2 > r {
                        break;
                    }
                }
                let [s, p, o] = ord.unpermute(l0, l1, l2);
                out.push(Triple::new(s, p, o));
                iter.seek(2, l2.wrapping_add(1));
            }
            iter.up(2);
            iter.seek(1, l1.wrapping_add(1));
        }
        iter.up(1);
        iter.seek(0, l0.wrapping_add(1));
    }
    Ok(out)
}

/// Extract the values bound by `pat` for the variables in `vars`, returning
/// one entry per variable in `vars` order.
fn project(pat: &TriplePattern, t: Triple, vars: &[Var]) -> Vec<TermId> {
    let mut out = Vec::with_capacity(vars.len());
    for v in vars {
        let val = match pat.position_of(*v) {
            Some(0) => t.s,
            Some(1) => t.p,
            Some(2) => t.o,
            _ => panic!("variable {v:?} not in pattern"),
        };
        out.push(val);
    }
    out
}

pub struct BatchIter<'src, S: TripleSource + ?Sized + 'src> {
    exec: BinaryHashExecutor<'src, S>,
    /// All output rows materialised eagerly — Stage-1 simplification. For
    /// Stage-2 we'll stream batches lazily.
    rows: std::vec::IntoIter<Vec<TermId>>,
    builder: BindingBatchBuilder,
    done: bool,
    pending_error: Option<WcojError>,
    /// Special case: zero output vars — emit one row per satisfied query.
    ground_match_remaining: usize,
}

impl<'src, S: TripleSource + ?Sized + 'src> BatchIter<'src, S> {
    fn new(exec: BinaryHashExecutor<'src, S>) -> Self {
        let mut pending_error = None;
        let mut rows: Vec<Vec<TermId>> = Vec::new();
        let mut ground_match_remaining = 0usize;

        match exec.eval(&exec.spec) {
            Ok((vars, all_rows)) => {
                if exec.out_vars.is_empty() {
                    ground_match_remaining = all_rows.len();
                } else {
                    let out_positions: Vec<usize> = exec
                        .out_vars
                        .iter()
                        .map(|v| vars.iter().position(|x| x == v).expect("out var missing"))
                        .collect();
                    rows = all_rows
                        .into_iter()
                        .map(|r| out_positions.iter().map(|&i| r[i]).collect())
                        .collect();
                }
            }
            Err(e) => pending_error = Some(e),
        }

        let builder = BindingBatchBuilder::new(exec.out_vars.clone());
        Self {
            exec,
            rows: rows.into_iter(),
            builder,
            done: false,
            pending_error,
            ground_match_remaining,
        }
    }
}

impl<'src, S: TripleSource + ?Sized + 'src> Iterator for BatchIter<'src, S> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.pending_error.take() {
            self.done = true;
            return Some(Err(e));
        }
        if self.done {
            return None;
        }
        if self.ground_match_remaining > 0 && self.exec.out_vars.is_empty() {
            let n = self.ground_match_remaining;
            self.ground_match_remaining = 0;
            self.done = true;
            let schema = self.builder.schema();
            return Some(
                RecordBatch::try_new_with_options(
                    schema,
                    Vec::new(),
                    &arrow::record_batch::RecordBatchOptions::new().with_row_count(Some(n)),
                )
                .map_err(WcojError::Arrow),
            );
        }
        loop {
            match self.rows.next() {
                Some(row) => {
                    if let Some(b) = self.builder.push_row(&row) {
                        return Some(Ok(b));
                    }
                }
                None => {
                    self.done = true;
                    return self.builder.finish().map(Ok);
                }
            }
        }
    }
}
