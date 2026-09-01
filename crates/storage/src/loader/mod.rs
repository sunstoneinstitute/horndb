//! Bulk loaders.
//!
//! Stage-1 streaming loaders for N-Triples, Turtle, and N-Quads (SPEC-02 F8),
//! all built on `oxttl` streaming parsers feeding the dictionary + tier in
//! batches of [`load_batch_triples`]. N-Quads routes each quad to the graph named by
//! its fourth term (SPEC-02 F7); N-Triples and Turtle load the default graph.
pub mod nquads;
pub mod ntriples;
pub mod parallel;
pub mod turtle;

pub use parallel::{
    load_buffer_triples, load_threads, max_slice_bytes, set_load_buffer_triples,
    DEFAULT_LOAD_BUFFER_TRIPLES, DEFAULT_MAX_SLICE_BYTES,
};

use crate::dictionary::{flush_intern_phases, Dictionary};
use crate::error::Result;
use crate::store::Store;
use crate::term::{GraphId, TermId};
use oxrdf::{NamedOrBlankNode, Term};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Default triples per tier insert across all loaders. The tier appends a
/// batch as one sorted run and merges the runs once, on the first read
/// (HDB-84), so this is a memory-vs-call-overhead knob, not an index-rebuild
/// knob: cost is near-flat in the batch size.
pub const DEFAULT_LOAD_BATCH_TRIPLES: usize = 65_536;

/// Resolved once, then cached: `HORNDB_LOAD_BATCH_TRIPLES` if set and
/// parseable, else [`DEFAULT_LOAD_BATCH_TRIPLES`]. `0` means "not yet read".
static LOAD_BATCH_TRIPLES: AtomicUsize = AtomicUsize::new(0);

/// Triples buffered before each tier insert.
///
/// Set it with `HORNDB_LOAD_BATCH_TRIPLES=<n>`, or from code with
/// [`set_load_batch_triples`]. It cannot change what a load produces — only
/// how the same rows are handed to the tier — which is what the batch-size
/// determinism test proves.
pub fn load_batch_triples() -> usize {
    match LOAD_BATCH_TRIPLES.load(Ordering::Relaxed) {
        0 => {
            let v = std::env::var("HORNDB_LOAD_BATCH_TRIPLES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n >= 1)
                .unwrap_or(DEFAULT_LOAD_BATCH_TRIPLES);
            LOAD_BATCH_TRIPLES.store(v, Ordering::Relaxed);
            v
        }
        v => v,
    }
}

/// Override [`load_batch_triples`] for this process, ignoring the environment.
pub fn set_load_batch_triples(triples: usize) {
    LOAD_BATCH_TRIPLES.store(triples.max(1), Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct LoadStats {
    pub triples: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub dictionary_size: u64,
}

/// Drive a stream of parsed quads into the store: intern each term, batch into
/// chunks of [`load_batch_triples`], and flush to the tier. Shared by every loader; each
/// format only differs in how it turns a parser item into a
/// `(graph, subject, predicate, object)` tuple (`bytes_read` is filled in by the
/// file-level entry points). The default graph uses
/// [`crate::term::DEFAULT_GRAPH`].
pub(crate) fn load_quads<I>(store: &Store, quads: I) -> Result<LoadStats>
where
    I: Iterator<Item = Result<(GraphId, Term, Term, Term)>>,
{
    load_quads_in_graphs(store, quads, |_, g| Ok(g))
}

/// [`load_quads`] for a format whose graph label still has to be resolved —
/// N-Quads, where `to_graph` interns the label and allocates its `GraphId`.
///
/// **`to_graph` runs on the consumer, inside the intern batch, not in the
/// iterator.** Interning a graph label allocates a dictionary id like any
/// other term, so resolving it while the parser runs ahead would give every
/// label in a batch its id before any of that batch's subjects — a different
/// dictionary from the one the same document gets through
/// [`nquads::load_nquads_slice`]. Doing it here keeps the per-quad
/// (graph, subject, predicate, object) order both paths share, which is what
/// `tests/parallel_loader.rs::assert_same_store` pins.
///
/// **Note the buffer this introduces on the streaming path (HDB-106):** up to
/// 8,192 parsed rows — owned `Term`s, moved not cloned — are held before each
/// intern batch, where the pre-HDB-106 loop interned each row as the parser
/// produced it. It is bounded and small (a fraction of the 1 MiB `BufReader`
/// this path already carries), and it exists for one reason only: it is the
/// granularity the `intern` phase is clocked at. It buys no parse overlap and
/// is not a document buffer.
pub(crate) fn load_quads_in_graphs<I, G, R>(
    store: &Store,
    items: I,
    to_graph: R,
) -> Result<LoadStats>
where
    I: Iterator<Item = Result<(G, Term, Term, Term)>>,
    R: Fn(&Store, G) -> Result<GraphId>,
{
    let mut sink = QuadSink::new(store);
    // Buffered in the same 8,192-item batches the parallel path uses, for one
    // reason: it is the granularity the `intern` phase is clocked at
    // ([`QuadSink::intern_batch`]). Interning is otherwise unchanged — same
    // terms, same order — and the buffer holds moved `Term`s, not clones.
    let mut pending: Vec<(G, Term, Term, Term)> = Vec::with_capacity(parallel::BATCH);
    for item in items {
        pending.push(item?);
        if pending.len() >= parallel::BATCH {
            sink.intern_batch(|s| drain_into(s, &mut pending, &to_graph))?;
        }
    }
    if !pending.is_empty() {
        sink.intern_batch(|s| drain_into(s, &mut pending, &to_graph))?;
    }
    sink.finish()
}

fn drain_into<G, R>(
    sink: &mut QuadSink<'_>,
    pending: &mut Vec<(G, Term, Term, Term)>,
    to_graph: &R,
) -> Result<()>
where
    R: Fn(&Store, G) -> Result<GraphId>,
{
    for (g, s, p, o) in pending.drain(..) {
        let g = to_graph(sink.store, g)?;
        sink.push(g, &s, &p, &o)?;
    }
    Ok(())
}

/// Sentinel for "this term was not interned when a parse thread probed for
/// it" in [`Probed::ids`].
///
/// `TermId(0)` is kind `Uri` with dictionary index 0, and dictionary indices
/// start at 1 ([`Dictionary::intern`] computes `reverse.len() + 1`), so no
/// dictionary ever issues it. That makes it a free sentinel — no `Option`, no
/// 8 extra bytes per term in a buffer that holds millions of them.
pub(crate) const UNRESOLVED: TermId = TermId(0);

/// One parsed row's subject, predicate and object after a parse thread has
/// probed each of them against the dictionary (HDB-106).
///
/// The probe is [`Dictionary::get`] — read-only, allocates no id. A term it
/// resolves keeps its id in `ids` and is **dropped on the parse thread**,
/// which shrinks the in-flight buffer and keeps the free on the thread that
/// did the allocation. A term it does not resolve travels in `terms` for the
/// consumer to intern.
///
/// Invariant: `terms[i].is_some()` exactly when `ids[i] == UNRESOLVED`.
///
/// **Term ids are unchanged by this**: see [`QuadSink::push_probed`].
pub(crate) struct Probed {
    terms: [Option<Term>; 3],
    ids: [TermId; 3],
}

impl Probed {
    /// Probe `(s, p, o)` against `dict`, keeping only the terms it could not
    /// resolve.
    pub(crate) fn probe(dict: &Dictionary, s: Term, p: Term, o: Term) -> Self {
        let mut terms = [Some(s), Some(p), Some(o)];
        let mut ids = [UNRESOLVED; 3];
        for (id, slot) in ids.iter_mut().zip(terms.iter_mut()) {
            let term = slot.as_ref().expect("all three slots start occupied");
            if let Some(hit) = dict.get(term) {
                *id = hit;
                *slot = None;
            }
        }
        Self { terms, ids }
    }
}

/// One batch as it travels from a parse thread to the consumer.
///
/// Two shapes, chosen once per 8,192-row batch rather than per row:
///
/// * `Raw` — a parse the probe cannot pay for
///   ([`parallel::should_probe`] said no): a single chunk, where there is no
///   other thread to move the lookup to and probing would be the consumer's own
///   lookup done twice, or 2–3 chunks, where the parse threads have no spare
///   capacity to absorb it. The rows are handed through exactly as they were
///   before HDB-106, so those paths pay nothing for the probe — no extra bytes
///   in the buffer, no extra work per row.
/// * `Probed` — a parse split at least [`parallel::MIN_PROBE_CHUNKS`] ways.
///   Each row carries what its parse thread resolved; the consumer allocates
///   ids only for the rest.
pub(crate) enum Batch<R, P> {
    Raw(Vec<R>),
    Probed(Vec<P>),
}

/// Intern + batch + flush, shared by the streaming (`load_quads`) and the
/// slice/parallel loaders. Both drive it from a single thread in document
/// order, so a document loaded either way gets the same term ids.
pub(crate) struct QuadSink<'a> {
    store: &'a Store,
    batch: Vec<(GraphId, TermId, TermId, TermId)>,
    batch_size: usize,
    total: u64,
    start: Instant,
    /// Nanoseconds charged to the `intern` load phase: the time spent inside
    /// [`QuadSink::intern_batch`], net of the tier flushes that happened
    /// there. One clock pair per parse batch, never per triple (SPEC-17 §5.4).
    intern_ns: u64,
    /// Nanoseconds inside `Tier::insert_quad_batch`, one clock pair per flush
    /// — once per `batch_size` rows, so ~150 pairs for a 10 M-triple load.
    /// Subtracted out of `intern_ns`; the tier reports its own phases.
    flush_ns: u64,
}

impl<'a> QuadSink<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        let batch_size = load_batch_triples();
        Self {
            store,
            // Cap the preallocation: a very large batch size is a "flush once"
            // request, not a request to reserve that much up front.
            batch: Vec::with_capacity(batch_size.min(1 << 20)),
            batch_size,
            total: 0,
            start: Instant::now(),
            intern_ns: 0,
            flush_ns: 0,
        }
    }

    /// Run one parse batch's worth of pushes and charge them to the `intern`
    /// load phase.
    ///
    /// The `intern` phase used to be reported as a residue — wall clock minus
    /// the counted phases — which meant nobody could optimise against it
    /// without also owning every unmetered thing it absorbed (HDB-96). This
    /// makes it a counter. It is still not clocked per triple: HDB-90 put one
    /// `Instant::now()` pair at 16.5 ns, so bracketing ~30 M intern calls
    /// would cost half a second to measure three. One pair per 8,192-item
    /// batch costs microseconds over a whole load.
    ///
    /// The tier flushes that fall inside the batch are clocked separately and
    /// subtracted, so `intern` means interning (plus the id tuple push), not
    /// interning plus whatever the tier did. What is left uncharged is the
    /// parse, which [`SinkTimer`] reports.
    pub(crate) fn intern_batch<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Self) -> Result<()>,
    {
        let t = Instant::now();
        let flush_before = self.flush_ns;
        let out = f(self);
        let elapsed = t.elapsed().as_nanos() as u64;
        self.intern_ns += elapsed.saturating_sub(self.flush_ns - flush_before);
        out
    }

    pub(crate) fn push(&mut self, g: GraphId, s: &Term, p: &Term, o: &Term) -> Result<()> {
        let (s_id, p_id, o_id) = self.store.dictionary().intern_triple(s, p, o)?;
        self.record(g, s_id, p_id, o_id)
    }

    /// [`QuadSink::push`] for a row a parse thread already probed.
    ///
    /// **Term ids are identical to what [`QuadSink::push`] would have
    /// assigned.** A probe that hit names an id the dictionary had already
    /// issued, and the dictionary is append-only, so interning that term again
    /// would return the same id. A probe that missed — including one that
    /// missed only because it raced the consumer — falls through to
    /// [`Dictionary::intern`] here, on this thread, in document order. So the
    /// *order in which new ids are allocated* is untouched, which is the
    /// property `tests/parallel_loader.rs::assert_same_store` pins.
    pub(crate) fn push_probed(&mut self, g: GraphId, probed: Probed) -> Result<()> {
        let Probed { terms, mut ids } = probed;
        let dict = self.store.dictionary();
        for (id, term) in ids.iter_mut().zip(terms) {
            if *id == UNRESOLVED {
                let term = term.expect("an unresolved slot always carries its term");
                *id = dict.intern(&term)?;
            }
        }
        self.record(g, ids[0], ids[1], ids[2])
    }

    fn record(&mut self, g: GraphId, s: TermId, p: TermId, o: TermId) -> Result<()> {
        self.batch.push((g, s, p, o));
        self.total += 1;
        if self.batch.len() >= self.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let t = Instant::now();
        let out = self.store.tier().insert_quad_batch(&self.batch);
        self.flush_ns += t.elapsed().as_nanos() as u64;
        // Clear only on success. A failed insert leaves the batch that failed
        // in place rather than discarding it, which is what the `?` this
        // replaced did.
        out?;
        self.batch.clear();
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<LoadStats> {
        if !self.batch.is_empty() {
            self.flush()?;
        }
        if self.total > 0 {
            horndb_metrics::metrics().storage.record_load_phase(
                horndb_metrics::labels::LoadPhase::Intern,
                Duration::from_nanos(self.intern_ns),
                self.total,
            );
        }
        // Publishes the `HORNDB_INTERN_PHASES=1` split, if it is on. No-op
        // otherwise.
        flush_intern_phases();
        Ok(LoadStats {
            triples: self.total,
            bytes_read: 0, // file-level caller overwrites this
            elapsed_ms: self.start.elapsed().as_millis() as u64,
            dictionary_size: self.store.dictionary().len() as u64,
        })
    }
}

/// Accumulates the calling thread's own time inside a slice load, so the
/// `parse` phase can be reported for a real `Store` load.
///
/// The slice loaders drain parsed batches on the calling thread and intern and
/// insert them there. Timing the drain per triple would cost more than it
/// measures (HDB-90 put one clock read at 16.5 ns), so the split is taken once
/// per 8,192-item parse batch: `sink` is the intern plus tier work, and
/// whatever is left of the wall clock is time the consumer spent waiting on,
/// or running, the parse.
///
/// At one thread that residue is the inline parse itself; above one thread it
/// is what the consumer still waits for after the parse threads have run
/// ahead. Either way it is the quantity `HORNDB_LOAD_THREADS` acts on.
pub(crate) struct SinkTimer {
    start: Instant,
    sink_ns: u64,
}

impl SinkTimer {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            sink_ns: 0,
        }
    }

    /// Run one batch's worth of sink work and charge it to the sink, not parse.
    pub(crate) fn sink<T>(&mut self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let t = Instant::now();
        let out = f();
        self.sink_ns += t.elapsed().as_nanos() as u64;
        out
    }

    /// Record everything not charged to the sink as the `parse` load phase.
    pub(crate) fn record_parse(self, rows: u64) {
        let parse_ns = (self.start.elapsed().as_nanos() as u64).saturating_sub(self.sink_ns);
        horndb_metrics::metrics().storage.record_load_phase(
            horndb_metrics::labels::LoadPhase::Parse,
            std::time::Duration::from_nanos(parse_ns),
            rows,
        );
    }
}

/// RDF 1.2's data model (oxrdf 0.3 with `rdf-12`) keeps subjects as the
/// 1.1-shaped `NamedOrBlankNode`: triple terms appear only in the object
/// position (oxrdf's `Term::Triple`). The match is exhaustive.
pub(crate) fn subject_to_term(s: NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b),
    }
}
