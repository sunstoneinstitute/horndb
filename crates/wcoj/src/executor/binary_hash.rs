//! Left-deep binary-hash-join executor.
//!
//! Two jobs: (1) execute BGPs of ≤3 patterns (where WCOJ overhead is not
//! worth paying), (2) serve as the bit-identical reference implementation
//! for the differential fuzzer (SPEC-03 acceptance #3).
//!
//! Algorithm: scan pattern 0 (full source materialised through `iter` over
//! its preferred ordering, filtering by bound positions). For each
//! subsequent pattern, build a hash table on the join keys (variables in
//! common with the running binding set) and probe.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::pattern::{Bgp, Term, TriplePattern, Var};
use crate::source::TripleSource;

pub struct BinaryHashExecutor<'src> {
    source: &'src dyn TripleSource,
    bgp: Arc<Bgp>,
    out_vars: Vec<Var>,
    cancel: CancelToken,
}

impl<'src> BinaryHashExecutor<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        out_vars: Vec<Var>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            source,
            bgp: Arc::new(bgp.clone()),
            out_vars,
            cancel,
        }
    }

    pub fn into_iter(self) -> BatchIter<'src> {
        BatchIter::new(self)
    }
}

/// All matching triples for a single pattern, materialised eagerly.
///
/// Stage-1 simplification: full scan of one ordering, filtering on bound
/// positions. SPEC-02 will offer a more selective access path; we don't
/// need it here.
fn scan_pattern<'src>(source: &'src dyn TripleSource, pat: &TriplePattern) -> Result<Vec<Triple>> {
    let ord = Ordering::Spo;
    let mut iter = source.iter(ord)?;
    let mut out = Vec::new();

    while let Some(s) = iter.peek(0) {
        if let Term::Bound(req_s) = pat.s {
            if s < req_s {
                iter.seek(0, req_s);
                continue;
            }
            if s > req_s {
                break;
            }
        }
        iter.open_level(1);
        while let Some(p) = iter.peek(1) {
            if let Term::Bound(req_p) = pat.p {
                if p < req_p {
                    iter.seek(1, req_p);
                    continue;
                }
                if p > req_p {
                    break;
                }
            }
            iter.open_level(2);
            while let Some(o) = iter.peek(2) {
                if let Term::Bound(req_o) = pat.o {
                    if o < req_o {
                        iter.seek(2, req_o);
                        continue;
                    }
                    if o > req_o {
                        break;
                    }
                }
                out.push(Triple::new(s, p, o));
                iter.seek(2, o.wrapping_add(1));
            }
            iter.up(2);
            iter.seek(1, p.wrapping_add(1));
        }
        iter.up(1);
        iter.seek(0, s.wrapping_add(1));
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

pub struct BatchIter<'src> {
    exec: BinaryHashExecutor<'src>,
    /// All output rows materialised eagerly — Stage-1 simplification. For
    /// Stage-2 we'll stream batches lazily.
    rows: std::vec::IntoIter<Vec<TermId>>,
    builder: BindingBatchBuilder,
    done: bool,
    pending_error: Option<WcojError>,
    /// Special case: ground BGP with zero output vars — emit one row per
    /// satisfied query.
    ground_match_remaining: usize,
}

impl<'src> BatchIter<'src> {
    fn new(exec: BinaryHashExecutor<'src>) -> Self {
        let mut pending_error = None;
        let mut rows: Vec<Vec<TermId>> = Vec::new();
        let mut ground_match_remaining = 0usize;

        let all_ground = exec.bgp.patterns.iter().all(|p| p.is_ground());
        if all_ground {
            let mut count = 1usize;
            for pat in &exec.bgp.patterns {
                match scan_pattern(exec.source, pat) {
                    Ok(v) if v.is_empty() => {
                        count = 0;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        pending_error = Some(e);
                        break;
                    }
                }
            }
            ground_match_remaining = count;
        } else if let Err(e) = (|| -> Result<()> {
            let first = &exec.bgp.patterns[0];
            let first_vars: Vec<Var> = exec
                .out_vars
                .iter()
                .filter(|v| first.position_of(**v).is_some())
                .copied()
                .collect();
            let triples = scan_pattern(exec.source, first)?;
            let mut cur_vars = first_vars.clone();
            let mut cur_rows: Vec<Vec<TermId>> = triples
                .iter()
                .map(|t| project(first, *t, &cur_vars))
                .collect();

            for pat in exec.bgp.patterns.iter().skip(1) {
                if let Err(e) = exec.cancel.check() {
                    return Err(e);
                }
                let pat_vars: Vec<Var> = exec
                    .out_vars
                    .iter()
                    .filter(|v| pat.position_of(**v).is_some())
                    .copied()
                    .collect();
                let join_keys: Vec<Var> = cur_vars
                    .iter()
                    .filter(|v| pat_vars.contains(v))
                    .copied()
                    .collect();

                let new_triples = scan_pattern(exec.source, pat)?;
                let new_rows: Vec<Vec<TermId>> = new_triples
                    .iter()
                    .map(|t| project(pat, *t, &pat_vars))
                    .collect();

                let pat_key_positions: Vec<usize> = join_keys
                    .iter()
                    .map(|v| pat_vars.iter().position(|x| x == v).unwrap())
                    .collect();
                let mut ht: HashMap<Vec<TermId>, Vec<Vec<TermId>>> = HashMap::new();
                for nr in &new_rows {
                    let key: Vec<TermId> = pat_key_positions.iter().map(|&i| nr[i]).collect();
                    ht.entry(key).or_default().push(nr.clone());
                }

                let cur_key_positions: Vec<usize> = join_keys
                    .iter()
                    .map(|v| cur_vars.iter().position(|x| x == v).unwrap())
                    .collect();
                let mut combined_vars = cur_vars.clone();
                let mut pat_extra_positions: Vec<usize> = Vec::new();
                for (i, v) in pat_vars.iter().enumerate() {
                    if !cur_vars.contains(v) {
                        combined_vars.push(*v);
                        pat_extra_positions.push(i);
                    }
                }

                let mut joined: Vec<Vec<TermId>> = Vec::new();
                for cr in &cur_rows {
                    let key: Vec<TermId> = cur_key_positions.iter().map(|&i| cr[i]).collect();
                    if let Some(matches) = ht.get(&key) {
                        for m in matches {
                            let mut row = cr.clone();
                            for &i in &pat_extra_positions {
                                row.push(m[i]);
                            }
                            joined.push(row);
                        }
                    }
                }
                cur_rows = joined;
                cur_vars = combined_vars;
            }

            let out_positions: Vec<usize> = exec
                .out_vars
                .iter()
                .map(|v| {
                    cur_vars
                        .iter()
                        .position(|x| x == v)
                        .expect("out var missing")
                })
                .collect();
            rows = cur_rows
                .into_iter()
                .map(|r| out_positions.iter().map(|&i| r[i]).collect())
                .collect();
            Ok(())
        })() {
            pending_error = Some(e);
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

impl<'src> Iterator for BatchIter<'src> {
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
