//! `GRAPH ?g { P }` — per-graph block evaluation (SPARQL 1.1 §18.2.2.2,
//! SPEC-28 D6).
//!
//! The spec evaluates `P` once per named graph with `?g` **free**, then joins
//! each result with the one-row solution `{?g → that graph}`. This operator is
//! that loop: it asks the executor for the graph list once
//! ([`Executor::named_graphs`] — the only place graphs may be enumerated),
//! then for each graph in turn builds `P`'s operator tree with the leaves
//! scoped to that one graph, streams its rows, joins the graph name on, and
//! drops the tree before moving to the next graph.
//!
//! Output columns are `P`'s (as `plan::pushdown::output_vars` computes them,
//! so the schema is fixed before the first graph is read) plus `?g`. The join
//! is: if `P` does not bind `?g`, append it bound to this graph; if it does, a
//! row survives only when its `?g` is unbound or equal to this graph, and is
//! then set to it.

use super::Op;
use crate::algebra::Var;
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor, NamedGraph, Row, Slot};
use crate::plan::PhysicalPlan;

pub struct PerGraphOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    var: Var,
    /// `P`. Rebuilt once per graph, so the operator owns it.
    inner: PhysicalPlan,
    /// The graph substitutions already in force (an enclosing `PerGraph`),
    /// which this operator extends with its own.
    outer: Vec<(Var, String)>,
    graphs: std::vec::IntoIter<NamedGraph>,
    /// The graph currently being read, and `P`'s operator tree over it.
    current: Option<(NamedGraph, Box<dyn Op + 'r>)>,
    schema: Vec<Var>,
    /// Index of `?g` in `schema`.
    graph_col: usize,
    /// True when `P` itself binds `?g` (`GRAPH ?g { ?g ?p ?o }`): the join is
    /// then a filter on `P`'s own column, not an appended one.
    bound_by_inner: bool,
}

impl<'r, E: Executor + ?Sized> PerGraphOp<'r, E> {
    pub fn new(
        rt: &'r Runtime<'r, E>,
        var: Var,
        inner: &PhysicalPlan,
        outer: &[(Var, String)],
    ) -> Result<Self> {
        let graphs = rt.exec().named_graphs(rt.named_set())?;
        // The output schema comes from the plan, not from the first graph's
        // rows: a graph whose block matches nothing must not narrow it.
        let mut names = crate::plan::pushdown::output_vars(inner);
        let bound_by_inner = names.iter().any(|n| n == var.name());
        if !bound_by_inner {
            names.push(var.name().to_owned());
        }
        let graph_col = names
            .iter()
            .position(|n| n == var.name())
            .expect("the graph variable is in the schema");
        Ok(Self {
            rt,
            var,
            inner: inner.clone(),
            outer: outer.to_vec(),
            graphs: graphs.into_iter(),
            current: None,
            schema: names.iter().map(|n| Var::new(n.as_str())).collect(),
            graph_col,
            bound_by_inner,
        })
    }

    /// Map one chunk of `P`'s rows over graph `graph` onto this operator's
    /// schema, applying the `{?g → graph}` join.
    fn join_graph_name(&self, graph: &NamedGraph, batch: Batch) -> Result<Vec<Row>> {
        let src: Vec<Option<usize>> = self.schema.iter().map(|v| batch.col(v.name())).collect();
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            if self.bound_by_inner {
                let own = src[self.graph_col]
                    .map(|i| &row.0[i])
                    .unwrap_or(&Slot::Unbound);
                if !matches!(own, Slot::Unbound)
                    && !Slot::eq(own, &graph.binding, |id| self.rt.exec().decode_term(id))?
                {
                    continue;
                }
            }
            out.push(Row(src
                .iter()
                .enumerate()
                .map(|(col, from)| {
                    if col == self.graph_col {
                        graph.binding.clone()
                    } else {
                        match from {
                            Some(i) => row.0[*i].clone(),
                            None => Slot::Unbound,
                        }
                    }
                })
                .collect()));
        }
        Ok(out)
    }
}

impl<'r, E: Executor + ?Sized> Op for PerGraphOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn may_emit_term(&self) -> Vec<bool> {
        // Over-approximation (`true` is always sound): `P`'s tree is not
        // built until the first graph is read, so its per-column provenance
        // is not known here. Consumers fall back to lexical keys.
        vec![true; self.schema.len()]
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        loop {
            if self.current.is_none() {
                let Some(graph) = self.graphs.next() else {
                    return Ok(None);
                };
                let mut binds = self.outer.clone();
                binds.push((self.var.clone(), graph.iri.clone()));
                let op = self.rt.build_scoped(&self.inner, &binds)?;
                self.current = Some((graph, op));
            }
            let (graph, op) = self.current.as_mut().expect("just built");
            match op.next()? {
                Some(chunk) => {
                    let graph = graph.clone();
                    let rows = self.join_graph_name(&graph, chunk)?;
                    // A chunk every row of which failed the join is not the
                    // end of the stream — pull the next one rather than
                    // yield an empty batch.
                    if !rows.is_empty() {
                        return Ok(Some(Batch {
                            schema: self.schema.clone(),
                            rows,
                        }));
                    }
                }
                // This graph is exhausted; drop its operator tree.
                None => self.current = None,
            }
        }
    }
}
