//! Public store facade.
//!
//! Composes a `Dictionary` with one `Tier` implementation. Stage 1 only
//! supports an in-memory tier; the constructor signature leaves room for
//! plugging in cold tiers later.

use crate::dictionary::Dictionary;
use crate::error::Result;
use crate::memory_tier::MemoryTier;
use crate::term::{GraphId, DEFAULT_GRAPH};
use crate::tier::{Tier, TierStats};
use oxrdf::Term;

#[derive(Debug, Clone, Copy)]
pub struct FootprintReport {
    pub triples: u64,
    pub bytes_estimated: u64,
    pub bytes_per_triple: f64,
}

pub struct Store {
    dictionary: Dictionary,
    tier: Box<dyn Tier>,
}

impl Store {
    pub fn in_memory() -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::new()),
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    pub fn tier(&self) -> &dyn Tier {
        self.tier.as_ref()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Insert into the default graph.
    pub fn insert_triples(&self, triples: &[(Term, Term, Term)]) -> Result<()> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let s_id = self.dictionary.intern(s)?;
            let p_id = self.dictionary.intern(p)?;
            let o_id = self.dictionary.intern(o)?;
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&quads)
    }

    /// Insert (graph, s, p, o) quads. Caller-supplied `GraphId`s must already
    /// have been interned via `intern_graph_uri`.
    pub fn insert_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<()> {
        let mut encoded = Vec::with_capacity(quads.len());
        for (g, s, p, o) in quads {
            let s_id = self.dictionary.intern(s)?;
            let p_id = self.dictionary.intern(p)?;
            let o_id = self.dictionary.intern(o)?;
            encoded.push((*g, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&encoded)
    }

    pub fn intern_graph_uri(&self, graph_uri: &Term) -> Result<GraphId> {
        let id = self.dictionary.intern(graph_uri)?;
        Ok(GraphId(id.0))
    }

    /// Scan a single predicate in the default graph, returning materialized
    /// (subject, object) `Term` pairs. Used by tests; production code should
    /// use the tier's columnar scan directly.
    pub fn scan_predicate_default_graph(&self, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        let p_id = self.dictionary.intern(predicate)?;
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        let pairs = mt
            .with_predicate(DEFAULT_GRAPH, p_id, |part| part.scan().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(pairs.len());
        for (s_id, o_id) in pairs {
            let s = self
                .dictionary
                .lookup(s_id)
                .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {s_id:?}")))?;
            let o = self
                .dictionary
                .lookup(o_id)
                .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {o_id:?}")))?;
            out.push((s, o));
        }
        Ok(out)
    }

    pub fn report_footprint(&self) -> FootprintReport {
        let stats = self.tier.stats();
        let bpt = if stats.triples == 0 {
            0.0
        } else {
            stats.bytes_estimated as f64 / stats.triples as f64
        };
        FootprintReport {
            triples: stats.triples,
            bytes_estimated: stats.bytes_estimated,
            bytes_per_triple: bpt,
        }
    }
}
