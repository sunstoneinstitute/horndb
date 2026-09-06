//! Public store facade.
//!
//! Composes a `Dictionary` with one `Tier` implementation. Stage 1 only
//! supports an in-memory tier; the constructor signature leaves room for
//! plugging in cold tiers later.

use crate::dictionary::Dictionary;
use crate::error::Result;
use crate::memory_tier::MemoryTier;
use crate::ordering::Ordering;
use crate::term::{GraphId, InternedQuad, TermId, DEFAULT_GRAPH};
use crate::tier::{ApplyReport, Tier, TierStats, TierWrite};
use crate::wal::{
    self, BatchRecord, InputRecord, Kind, Quad, Record, RecoveredInputs, SyncPolicy, Wal,
};
use oxrdf::Term;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// A fresh, process-unique directory under the OS temp dir, for cold files
/// belonging to a store with no directory of its own (`Store::in_memory` and
/// friends). Not cleaned up on drop: cold placement already assumes a
/// throwaway directory (`Store::open_with` deletes `<dir>/cold` on every
/// reopen), and there is no existing "temp dir owned by a production struct"
/// pattern in this crate to follow instead — `tempfile::TempDir` is a
/// dev-dependency only. See HDB-177 review notes for the tradeoff.
fn temp_cold_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    std::env::temp_dir().join(format!("horndb-cold-{}-{n}", std::process::id()))
}

#[derive(Debug, Clone, Copy)]
pub struct FootprintReport {
    pub triples: u64,
    pub bytes_estimated: u64,
    pub bytes_per_triple: f64,
}

pub struct Store {
    dictionary: Dictionary,
    tier: Box<dyn TierWrite>,
    /// Counts documents loaded into this store (HDB-113 blank-node scoping:
    /// see [`Store::next_bnode_doc_tag`]).
    bnode_doc_tag: AtomicU64,
    /// The write-ahead log of a store opened with [`Store::open`] (SPEC-25
    /// S3); `None` for an in-memory store. Its mutex serializes every logged
    /// write with the append that precedes it, so log order is commit order.
    wal: Option<Mutex<Wal>>,
    /// Where [`Store::demote`] / [`Store::demote_all`] write cold files
    /// (SPEC-25 S5): `<dir>/cold` for a durable store, so it lines up with the
    /// directory [`Store::open_with`] cleans on reopen; a fresh throwaway
    /// directory for an in-memory store (see [`temp_cold_dir`]). Exactly one
    /// place — this field's construction — knows the convention.
    cold_dir: PathBuf,
}

impl Store {
    pub fn in_memory() -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::new()),
            bnode_doc_tag: AtomicU64::new(0),
            wal: None,
            cold_dir: temp_cold_dir(),
        }
    }

    /// An in-memory tier over a dictionary the caller built — the way to
    /// reopen a store on a persisted dictionary (SPEC-25 S2):
    /// `Store::with_dictionary(Dictionary::open(path)?)`. The tier starts
    /// empty; ids the dictionary already issued keep their meaning, so
    /// re-loading the corpus (or a snapshot) allocates no new ids for terms
    /// the base holds.
    pub fn with_dictionary(dictionary: Dictionary) -> Self {
        let bnode_doc_tag = AtomicU64::new(dictionary.base_bnode_doc_tag());
        Self {
            dictionary,
            tier: Box::new(MemoryTier::new()),
            bnode_doc_tag,
            wal: None,
            cold_dir: temp_cold_dir(),
        }
    }

    /// Open a durable store under `dir` (SPEC-25 S3) with the default
    /// [`SyncPolicy::EveryBatch`]. Creates the directory on first use;
    /// otherwise reopens the last checkpoint's dictionary base and replays
    /// the write-ahead log past it, so the store comes back with the same
    /// term ids, the same visible quads, and the same commit version it had
    /// when the last durably-appended batch was written. Compaction is not
    /// logged, so rows an explicit `compact()` had reclaimed may be back as
    /// (invisible) dead rows. A torn tail record is dropped; a corrupt record
    /// before the tail is [`crate::StorageError::Wal`].
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with(dir, SyncPolicy::default())
    }

    /// [`Store::open`] with an explicit fsync policy — see [`SyncPolicy`] for
    /// each policy's data-loss window.
    pub fn open_with(dir: &Path, policy: SyncPolicy) -> Result<Self> {
        let mut log = Wal::open(dir, policy)?;
        let gen = log.generation();
        let dictionary = if gen == 0 {
            Dictionary::new()
        } else {
            Dictionary::open(&log.dict_path(gen))?
        };
        // ponytail: cold placement is not durable. Nothing records which
        // predicates were demoted, so replay rebuilds every partition warm and
        // any leftover cold file is stale — drop the whole directory rather
        // than leave orphans behind. Durable placement needs a manifest entry;
        // the placement-policy task re-demotes after a reopen.
        let _ = std::fs::remove_dir_all(dir.join("cold"));
        let mut store = Self::with_dictionary(dictionary);
        store.cold_dir = dir.join("cold");
        let mut seen_batch = false;
        let mut recovered = RecoveredInputs::default();
        log.replay(|rec| store.replay(rec, &mut seen_batch, &mut recovered))?;
        log.logged_len = store.dictionary.len() as u64;
        log.recovered = recovered;
        store.wal = Some(Mutex::new(log));
        Ok(store)
    }

    fn replay(
        &self,
        rec: Record,
        seen_batch: &mut bool,
        recovered: &mut RecoveredInputs,
    ) -> Result<()> {
        let rec: BatchRecord = match rec {
            Record::Batch(b) => b,
            // SPEC-24 S5: circuit inputs replay into the circuit, not the
            // tier. A tick marker closes the batch before it.
            Record::Input(i) => {
                recovered.push_input(i);
                return Ok(());
            }
            Record::TickCommit => {
                recovered.close_tick();
                return Ok(());
            }
        };
        for (i, term) in rec.dict.iter().enumerate() {
            self.dictionary
                .replay_append(rec.dict_first + i as u64, term)?;
        }
        self.bnode_doc_tag
            .fetch_max(rec.bnode_doc_tag, AtomicOrdering::Relaxed);
        let mt = self.memory_tier();
        let cur = mt.version();
        let bad = |what: &str| {
            crate::StorageError::Wal(format!(
                "{what} record at version {} after version {cur}",
                rec.version
            ))
        };
        match rec.kind {
            Kind::Checkpoint => {
                if *seen_batch || (cur != 0 && cur != rec.version) {
                    return Err(bad("checkpoint"));
                }
                if !rec.adds.is_empty() {
                    mt.insert_at(&rec.adds, Some(rec.version))?;
                }
                mt.set_version(rec.version);
            }
            Kind::Insert | Kind::Apply => {
                *seen_batch = true;
                if rec.version != cur + 1 {
                    return Err(bad("batch"));
                }
                if rec.kind == Kind::Insert {
                    mt.insert_at(&rec.adds, Some(rec.version))?;
                } else {
                    mt.apply_at(&rec.dels, &rec.adds, Some(rec.version))?;
                }
            }
            // `decode` returns these as `Record::Input` / `Record::TickCommit`,
            // never inside a `BatchRecord`.
            Kind::Input | Kind::TickCommit => return Err(bad("circuit")),
        }
        Ok(())
    }

    /// Append a record for the batch about to be written, then run the
    /// write. The record carries every dictionary index interned since the
    /// last record and the version the write will mint (`current + 1`; a
    /// net-empty apply leaves the clock alone, live and on replay alike).
    /// Without a log this is just the write.
    fn logged<T>(
        &self,
        kind: Kind,
        dels: &[Quad],
        adds: &[Quad],
        write: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(log) = &self.wal else {
            return write();
        };
        if dels.is_empty() && adds.is_empty() {
            return write();
        }
        let mut log = log.lock();
        self.append_record(&mut log, kind, dels, adds)?;
        write()
    }

    fn append_record(&self, log: &mut Wal, kind: Kind, dels: &[Quad], adds: &[Quad]) -> Result<()> {
        let terms = self.dictionary.terms_after(log.logged_len)?;
        let body = wal::encode(
            kind,
            self.memory_tier().version() + 1,
            self.bnode_doc_tag.load(AtomicOrdering::Relaxed),
            log.logged_len + 1,
            &terms,
            dels,
            adds,
        )?;
        log.append(&body)?;
        log.logged_len += terms.len() as u64;
        Ok(())
    }

    /// Checkpoint a store opened with [`Store::open`]: flush the dictionary
    /// base, write the rows visible now as the head of a new log generation,
    /// switch `MANIFEST` to it, and drop the old generation. Atomic — a crash
    /// anywhere before the `MANIFEST` rename leaves the previous generation
    /// intact and complete. Excludes writers for the duration (the dictionary
    /// flush is O(terms)). Errors on an in-memory store.
    ///
    /// Pre-checkpoint history is dropped: after a reopen every row the
    /// checkpoint carried has `begin` = the checkpoint's commit version and
    /// dead rows are gone, so an as-of read below that version sees an empty
    /// store. Batches after the checkpoint keep their exact stamps.
    pub fn checkpoint(&self) -> Result<()> {
        let mut log = self.wal_or_err()?.lock();
        let gen = log.generation() + 1;
        let stats = self.flush_dictionary(&log.dict_path(gen))?;
        let snap = self.snapshot();
        let version = snap.version();
        let tag = self.bnode_doc_tag.load(AtomicOrdering::Relaxed);
        let mut file = log.start_generation(gen)?;
        // ponytail: 1M-row chunks keep one record well under the u32 frame;
        // the whole row set still streams through one Vec at a time.
        const CHUNK: usize = 1 << 20;
        let mut chunk: Vec<Quad> = Vec::new();
        for g in snap.graphs() {
            for p in snap.tier.predicates(g) {
                let rows = snap
                    .tier
                    .with_predicate(g, p, |part| part.scan_at(version).collect::<Vec<_>>())
                    .unwrap_or_default();
                for (s, o) in rows {
                    chunk.push((g, s, p, o));
                    if chunk.len() == CHUNK {
                        Wal::write_checkpoint_record(&mut file, version, tag, &chunk)?;
                        chunk.clear();
                    }
                }
            }
        }
        // Always one record, so an emptied store still restores its clock.
        Wal::write_checkpoint_record(&mut file, version, tag, &chunk)?;
        drop(snap);
        log.commit_generation(gen, file, stats.slots)
    }

    /// Append one circuit input (ADR-0018 `Input`, SPEC-24 S5) — a user
    /// assert or retract the circuit has not drained into a tick yet. Durable
    /// per the store's [`SyncPolicy`]. Errors on an in-memory store.
    pub fn log_input(&self, rec: &InputRecord) -> Result<()> {
        self.wal_or_err()?.lock().append(&wal::encode_input(rec))
    }

    /// Mark a tick boundary (ADR-0018 `TickCommit`): every input up to
    /// `last_seq` is drained, at the store's current commit version. Always
    /// syncs, so a tick is durable under any [`SyncPolicy`] — the "per-tick"
    /// fsync setting is `SyncPolicy::Every` plus this call. Errors on an
    /// in-memory store.
    pub fn log_tick_commit(&self, last_seq: u64) -> Result<()> {
        let version = self.memory_tier().version();
        let mut log = self.wal_or_err()?.lock();
        log.append(&wal::encode_tick_commit(version, last_seq))?;
        log.sync()
    }

    /// Take the circuit inputs this store recovered at open (ADR-0018): the
    /// batches a tick marker closed, plus the un-ticked tail. Empty after the
    /// first call, and empty for a store with no log.
    pub fn take_recovered_inputs(&self) -> RecoveredInputs {
        match &self.wal {
            Some(log) => std::mem::take(&mut log.lock().recovered),
            None => RecoveredInputs::default(),
        }
    }

    /// Whether this store has un-replayed circuit inputs from its last open.
    pub fn has_recovered_inputs(&self) -> bool {
        self.wal
            .as_ref()
            .is_some_and(|l| !l.lock().recovered.is_empty())
    }

    /// Force the write-ahead log to stable storage now — the explicit sync
    /// under [`SyncPolicy::Every`]. Errors on an in-memory store.
    pub fn sync_wal(&self) -> Result<()> {
        self.wal_or_err()?.lock().sync()
    }

    fn wal_or_err(&self) -> Result<&Mutex<Wal>> {
        self.wal
            .as_ref()
            .ok_or_else(|| crate::StorageError::Wal("store has no write-ahead log".into()))
    }

    fn memory_tier(&self) -> &MemoryTier {
        self.tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier")
    }

    /// `Tier::insert_quad_batch` through the write-ahead log — the bulk
    /// loaders' flush path.
    pub(crate) fn insert_quad_batch(&self, quads: &[Quad]) -> Result<()> {
        self.logged(Kind::Insert, &[], quads, || {
            self.tier.insert_quad_batch(quads)
        })
    }

    /// [`Dictionary::flush`] with this store's next blank-node document tag,
    /// so a store reopened on the file scopes its next document's blank
    /// nodes away from every document already in the base.
    pub fn flush_dictionary(&self, path: &std::path::Path) -> Result<crate::BaseStats> {
        self.dictionary
            .flush(path, self.bnode_doc_tag.load(AtomicOrdering::Relaxed))
    }

    /// In-memory store with a custom hot-predicate threshold (SPEC-02 F4):
    /// predicates with at least `hot_threshold` live rows materialise the
    /// object-major layout eagerly rather than on the first object-major read.
    /// A partition holds two physical layouts, not six — they serve the six
    /// trie orderings between them (`crate::ordering`).
    ///
    /// [`Store::in_memory`] takes the process-wide value instead
    /// ([`crate::hot_threshold`], settable with `HORNDB_HOT_THRESHOLD`); this
    /// constructor overrides it for one store.
    pub fn in_memory_with_hot_threshold(hot_threshold: usize) -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::with_hot_threshold(hot_threshold)),
            bnode_doc_tag: AtomicU64::new(0),
            wal: None,
            cold_dir: temp_cold_dir(),
        }
    }

    /// A fresh tag for one document load into this store (HDB-113). Blank
    /// node labels are scoped to one document in N-Triples/Turtle/N-Quads,
    /// but `oxttl` emits them verbatim, so nothing stops `_:b1` from two
    /// different documents landing on the same node once both are loaded
    /// into one store. Every loader entry point (and SPARQL `LOAD`) calls
    /// this once per document and renames every blank node label it parses
    /// with [`crate::loader::scope_blank_node`], so two labels only collide
    /// when they share a tag — i.e. came from the same document load.
    ///
    /// Scoped per store, not process-wide: two stores that each load "their
    /// first document" independently (e.g. a parallel-parse-vs-serial-parse
    /// comparison of the same bytes into two separate stores) see the same
    /// first tag, so document-scoped renaming does not by itself change
    /// which store a term lands in. Persisted by [`Store::flush_dictionary`]
    /// and restored by [`Store::with_dictionary`].
    pub fn next_bnode_doc_tag(&self) -> u64 {
        self.bnode_doc_tag.fetch_add(1, AtomicOrdering::Relaxed)
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    /// Read-only view of the tier. Writes go through the store's entry points
    /// (`insert_quads`, `retract_quads`, `apply_quads`, [`Store::apply_quad_ids`])
    /// so the write-ahead log sees every batch — [`crate::TierWrite`] is not
    /// reachable from here.
    pub fn tier(&self) -> &dyn Tier {
        self.tier.as_ref()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Begin a read transaction: pin a stable, internally-consistent snapshot of
    /// the store (SPEC-02 copy-on-write snapshots — the Stage-1 substitute for
    /// per-tuple MVCC). Concurrent writers append to a new snapshot and never
    /// disturb the pinned view; it stays readable until dropped. The dictionary
    /// is append-only, so term ids in the pinned view never change meaning even
    /// as new terms are interned by other transactions.
    pub fn snapshot(&self) -> StoreSnapshot<'_> {
        StoreSnapshot {
            tier: self.pin(),
            dictionary: &self.dictionary,
        }
    }

    /// Pin the current tier state as an owned handle, detached from the
    /// dictionary borrow [`Store::snapshot`] carries. A caller that must keep
    /// one read version alive across many reads holds this and re-opens it
    /// with [`Store::snapshot_at`]; the pin is released when it drops.
    pub fn pin(&self) -> crate::memory_tier::PinnedSnapshot {
        self.memory_tier().snapshot()
    }

    /// Re-open a pin as a full read view: same tier state, same version, no
    /// re-read of the live pointer. This is what keeps every read of one
    /// query on a single commit version even as writers commit newer ones.
    pub fn snapshot_at(&self, pin: &crate::memory_tier::PinnedSnapshot) -> StoreSnapshot<'_> {
        StoreSnapshot {
            tier: pin.repin(),
            dictionary: &self.dictionary,
        }
    }

    /// Insert into the default graph.
    pub fn insert_triples(&self, triples: &[(Term, Term, Term)]) -> Result<()> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let (s_id, p_id, o_id) = self.dictionary.intern_triple(s, p, o)?;
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.insert_quad_batch(&quads)
    }

    /// Insert (graph, s, p, o) quads (SPEC-28 S6: a thin wrapper over
    /// [`Store::apply_quads`] with no deletions). Caller-supplied `GraphId`s
    /// must already have been interned via `intern_graph_uri`. Returns the
    /// number of quads actually inserted — a quad already visible is a
    /// counted no-op, not double-counted.
    pub fn insert_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        Ok(self.apply_quads(&[], quads)?.inserted)
    }

    /// Retract triples from the default graph (SPEC-25 S1). Returns the number
    /// of tuples actually retracted. Terms are looked up, not interned: a
    /// triple mentioning a term that was never inserted retracts nothing (the
    /// dictionary is append-only and a read/delete transaction must not mutate
    /// it).
    pub fn retract_triples(&self, triples: &[(Term, Term, Term)]) -> Result<usize> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue; // an un-interned term was never stored, so nothing to retract
            };
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.logged(Kind::Apply, &quads, &[], || {
            self.tier.retract_quad_batch(&quads)
        })
    }

    /// Retract (graph, s, p, o) quads (SPEC-28 S6: a thin wrapper over
    /// [`Store::apply_quads`] with no insertions). `GraphId`s must already
    /// have been interned via `intern_graph_uri`. See [`Store::retract_triples`]
    /// for the term-lookup (not intern) semantics.
    pub fn retract_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        Ok(self.apply_quads(quads, &[])?.retracted)
    }

    /// Apply a combined batch of deletions and insertions as one commit
    /// version (SPEC-28 S6, the store boundary a future change-feed
    /// materializer builds on). Deletions apply before insertions, so a
    /// delete+insert of the same quad within one batch ends present.
    /// `GraphId`s must already have been interned via `intern_graph_uri`.
    /// Deletion terms are looked up, not interned (mirroring
    /// [`Store::retract_quads`]: a term never seen retracts nothing);
    /// insertion terms are interned. A batch whose net effect is empty does
    /// not bump the store's commit version.
    pub fn apply_quads(
        &self,
        dels: &[(GraphId, Term, Term, Term)],
        adds: &[(GraphId, Term, Term, Term)],
    ) -> Result<ApplyReport> {
        let mut del_ids = Vec::with_capacity(dels.len());
        for (g, s, p, o) in dels {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue;
            };
            del_ids.push(InternedQuad::from_ids(*g, s_id, p_id, o_id));
        }
        let mut add_ids = Vec::with_capacity(adds.len());
        // One clock read on each side of the loop; the loop body touches no
        // metric handle (SPEC-17 §5.4).
        let t_intern = std::time::Instant::now();
        for (g, s, p, o) in adds {
            add_ids.push(self.dictionary.intern_quad(*g, s, p, o)?);
        }
        if !adds.is_empty() {
            horndb_metrics::metrics().storage.record_load_phase(
                horndb_metrics::labels::LoadPhase::Intern,
                t_intern.elapsed(),
                adds.len() as u64,
            );
        }
        self.apply_quad_ids(&del_ids, &add_ids)
    }

    /// [`Store::apply_quads`] for callers that already hold interned ids.
    ///
    /// A bulk loader that has to intern anyway — to deduplicate before the
    /// write, as `HornBackend` does — would otherwise hand the terms back for
    /// a second, identical dictionary lookup (HDB-87). Passing
    /// [`InternedQuad`]s skips that pass and the term buffer behind it. Same
    /// commit semantics as [`Store::apply_quads`]: deletions before
    /// insertions, one commit version, a net-empty batch does not bump it.
    /// Nothing is interned here, so no `intern` load phase is recorded.
    ///
    /// **Caller requirement: every id must come from *this* store's
    /// dictionary** — `self.dictionary().intern_quad(..)`, and a `GraphId`
    /// from `self.intern_graph_uri(..)`. [`InternedQuad`] proves an id was
    /// interned, not *where*, and ids are only meaningful against the
    /// dictionary that issued them; a quad interned against another store
    /// writes rows naming whatever terms those indices happen to hold here.
    /// Staleness is not a concern in the other direction — the dictionary is
    /// append-only, so an id it issued stays valid. A `debug_assert!` catches
    /// the obvious violations (see [`Dictionary::issued`]); the release path
    /// pays nothing, which is the point of this entry point.
    pub fn apply_quad_ids(
        &self,
        dels: &[InternedQuad],
        adds: &[InternedQuad],
    ) -> Result<ApplyReport> {
        debug_assert!(
            dels.iter().chain(adds).all(|q| self.issued_here(*q)),
            "apply_quad_ids: quad carries ids this store's dictionary never issued"
        );
        let (dels, adds) = (
            InternedQuad::peel_slice(dels),
            InternedQuad::peel_slice(adds),
        );
        self.logged(Kind::Apply, dels, adds, || {
            self.tier.apply_quad_batch(dels, adds)
        })
    }

    /// Every id in `q` is one this store's dictionary issued. Debug-only
    /// guard for [`Store::apply_quad_ids`] — it cannot distinguish two
    /// dictionaries that both hold the index, so it catches mistakes, not
    /// adversaries.
    fn issued_here(&self, q: InternedQuad) -> bool {
        let g = q.graph();
        (g == DEFAULT_GRAPH || self.dictionary.issued(TermId(g.0)))
            && self.dictionary.issued(q.subject())
            && self.dictionary.issued(q.predicate())
            && self.dictionary.issued(q.object())
    }

    /// Insert already-interned quads (SPEC-28 S6): a thin wrapper over
    /// [`Store::apply_quad_ids`] with no deletions, and the id-based twin of
    /// [`Store::insert_quads`]. Returns the number of quads actually inserted.
    /// Carries the same caller requirement: the ids must come from this
    /// store's dictionary.
    pub fn insert_quad_ids(&self, adds: &[InternedQuad]) -> Result<usize> {
        Ok(self.apply_quad_ids(&[], adds)?.inserted)
    }

    /// Reclaim physically-dead rows (`end <= min pinned version`) across the
    /// tier (SPEC-25 S1), then sweep the dictionary terms those rows were the
    /// last mention of (HDB-121). Without this, compaction is only reachable
    /// from tests that construct a `MemoryTier` directly.
    ///
    /// **Precondition on the dictionary sweep:** no thread may be holding a
    /// `TermId` it interned but has not yet installed rows for. Every
    /// `Store` write path interns and installs inside one call and the sweep
    /// bails if a write commits underneath it, so the exposure is the
    /// id-based entry points (`intern_graph_uri` / `Dictionary::intern_quad`
    /// followed by a later `insert_quad_ids`) and the bulk loaders, which
    /// intern on parse threads. Compaction is an explicit, quiesced
    /// maintenance call (HDB-63); do not wire it to a timer without closing
    /// that gap first. Rows are reclaimed either way — only the dictionary
    /// sweep carries this precondition.
    pub fn compact(&self) {
        let mt = self.memory_tier();
        mt.compact();
        // A WAL-backed store logs the not-yet-recorded dictionary appends
        // first (a rows-less record), so the sweep can never free an index
        // the log has not seen. If that append fails, rows are still
        // reclaimed and only the dictionary sweep is skipped.
        let logged = match &self.wal {
            Some(log) => self.append_record(&mut log.lock(), Kind::Apply, &[], &[]),
            None => Ok(()),
        };
        if logged.is_ok() {
            self.gc_dictionary(mt);
        }
    }

    /// Mark-and-sweep the dictionary against the rows the tier still holds.
    ///
    /// Mark, not refcount: a refcount would have to be maintained on every
    /// insert, retract and partition rebuild — including rows carried forward
    /// through MVCC history — to save a walk that only ever runs beside a
    /// compaction that has just walked the same rows anyway.
    ///
    /// The liveness bound is the tier's own: `compact()` keeps every row with
    /// `end > min_pinned`, so marking the rows that survive it marks
    /// everything any pinned reader can still resolve. Terms interned after
    /// `marks` was sized are past its end and are skipped; a write that lands
    /// while marking bumps the version and aborts the sweep.
    fn gc_dictionary(&self, mt: &MemoryTier) {
        let slots = self.dictionary.len();
        if slots == 0 {
            return;
        }
        let mut marks = vec![false; slots];
        let snap = mt.snapshot();
        let version = snap.version();
        snap.for_each_term_id(|bits| {
            let id = TermId(bits);
            if id.kind() == crate::term::TermKind::InlineInt {
                return; // value-encoded, never allocated
            }
            let idx = id.payload();
            if idx >= 1 && (idx as usize) <= marks.len() {
                marks[idx as usize - 1] = true;
            }
        });
        drop(snap);
        if mt.version() != version {
            return; // a write landed mid-mark; its terms may be unmarked
        }
        self.dictionary.gc(&marks);
    }

    /// Where this store's cold files live (SPEC-25 S5) — `<dir>/cold` for a
    /// durable store opened with [`Store::open`], a throwaway directory under
    /// the OS temp dir for an in-memory one.
    pub fn cold_dir(&self) -> &Path {
        &self.cold_dir
    }

    /// Demote one predicate partition to a cold, memory-mapped file under
    /// [`Self::cold_dir`] (SPEC-25 S5). Placement is by hand for now — no
    /// policy picks the victim. Not a logical write: see [`MemoryTier::demote`].
    ///
    /// Cold placement is **not durable**: [`Store::open`] deletes `<dir>/cold`
    /// and replays every partition warm.
    pub fn demote(&self, g: GraphId, p: TermId) -> Result<bool> {
        self.memory_tier().demote(g, p, &self.cold_dir)
    }

    /// Bring one cold partition back into memory and delete its file.
    pub fn promote(&self, g: GraphId, p: TermId) -> Result<bool> {
        self.memory_tier().promote(g, p)
    }

    /// Demote every warm partition in every graph, in `(graph, predicate)` id
    /// order, into [`Self::cold_dir`]. Returns how many were actually demoted
    /// (a partition with no visible rows is skipped).
    pub fn demote_all(&self) -> Result<usize> {
        let mt = self.memory_tier();
        let snap = mt.snapshot();
        let mut targets: Vec<(GraphId, TermId)> = snap
            .graphs()
            .into_iter()
            .flat_map(|g| snap.predicates(g).into_iter().map(move |p| (g, p)))
            .collect();
        drop(snap);
        targets.sort_by_key(|(g, p)| (g.0, p.0));
        let mut demoted = 0usize;
        for (g, p) in targets {
            if mt.demote(g, p, &self.cold_dir)? {
                demoted += 1;
            }
        }
        Ok(demoted)
    }

    /// Run one placement round over every partition: demote the ones that
    /// have gone unread for `cfg.demote_after_idle_rounds` rounds, promote the
    /// cold ones that were read (SPEC-25 S5). See
    /// [`MemoryTier::rebalance`](crate::MemoryTier::rebalance) for the rules.
    ///
    /// The trigger is explicit, exactly like [`Self::compact`] — a caller (a
    /// server timer, a test, the harness) decides the cadence. `hints` biases
    /// placement toward keeping named partitions warm and is additive: an
    /// empty [`PlacementHints`] gives the stats-only placement.
    pub fn rebalance(
        &self,
        cfg: &crate::tiering::TieringConfig,
        hints: &crate::tiering::PlacementHints,
    ) -> Result<crate::tiering::RebalanceReport> {
        self.memory_tier().rebalance(cfg, hints)
    }

    pub fn intern_graph_uri(&self, graph_uri: &Term) -> Result<GraphId> {
        let id = self.dictionary.intern(graph_uri)?;
        Ok(GraphId(id.0))
    }

    /// Scan a single predicate in `g`, returning materialized (subject,
    /// object) `Term` pairs, subject-major. Used by tests; production code
    /// should use the tier's columnar scan directly.
    pub fn scan_predicate(&self, g: GraphId, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        self.snapshot().scan_predicate(g, predicate)
    }

    /// Scan a single predicate in the default graph in the requested index
    /// ordering (SPEC-02 F4), returning materialized `(subject, predicate,
    /// object)` `Term` triples. Rows come back in the global order implied by
    /// `ord` (the predicate is constant within a partition, so the ordering is
    /// determined by the subject/object axis). For object-major orderings on a
    /// cold predicate the layout is materialised lazily on first call.
    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        self.snapshot().scan_predicate_ordered(predicate, ord)
    }

    /// The top-`n` predicates in the default graph by triple count (descending),
    /// as `(predicate Term, triple_count)`. Used to demonstrate SPEC-02
    /// acceptance #6 (top predicates queryable in all six orderings).
    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        self.snapshot().top_predicates(n)
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

    /// Dump every default-graph triple as raw `TermId`s, in arbitrary order,
    /// from a single pinned snapshot (internally consistent even under
    /// concurrent writes). O(triples) and materialized — intended for snapshot
    /// builders, not hot paths.
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        self.snapshot().scan_all_term_ids()
    }

    /// True if any non-default graph holds at least one triple. Snapshot
    /// export covers named graphs (SPEC-25 S4), so this is a plain predicate
    /// on store shape, not a guard.
    pub fn has_named_graph_data(&self) -> bool {
        self.snapshot().has_named_graph_data()
    }

    /// Export every graph — default plus named — to a writer in the
    /// HDT-derived snapshot format (SPEC-02 F9, SPEC-25 S4). See
    /// `crate::snapshot`.
    pub fn export_snapshot<W: std::io::Write>(
        &self,
        w: &mut W,
    ) -> Result<crate::snapshot::SnapshotStats> {
        crate::snapshot::export_snapshot(self, w)
    }

    /// Import a snapshot into this store, quads and all.
    pub fn import_snapshot<R: std::io::Read>(&self, r: &mut R) -> Result<()> {
        crate::snapshot::import_snapshot_into(self, r)
    }
}

/// A pinned, internally-consistent read view of a [`Store`] (SPEC-02
/// copy-on-write snapshot). Holds an `Arc` to the immutable tier state captured
/// at [`Store::snapshot`] time plus a borrow of the append-only dictionary for
/// term materialization. Cheap to create; cheap to drop.
pub struct StoreSnapshot<'a> {
    tier: crate::memory_tier::PinnedSnapshot,
    dictionary: &'a Dictionary,
}

impl StoreSnapshot<'_> {
    // --- whole-store ---

    /// Number of triples visible in this pinned view, across ALL graphs
    /// (SPEC-28 S2). Equivalent to `triple_count() as usize`. The old
    /// default-graph-scoped contract moved to [`Self::graph_len`] — see the
    /// `snapshot_len_is_whole_store` test for why the inversion is safe.
    pub fn len(&self) -> usize {
        self.triple_count() as usize
    }

    /// True if this pinned view has no visible triples in any graph. See
    /// [`Self::len`] for scope.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visible triples across all graphs. [`Self::len`] is the `usize` alias;
    /// [`Self::graph_len`] is the per-graph form.
    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// The snapshot id (monotonic tier version) this view is pinned to.
    pub fn version(&self) -> u64 {
        self.tier.version()
    }

    /// The pinned tier state as a cloneable `Arc`, for a reader that outlives
    /// this borrow — `horndb-sparql`'s direct partition `TripleSource` holds
    /// one for the length of a query (HDB-120). Compaction never mutates a
    /// published `TierSnapshot` (it swaps in freshly built partitions), so an
    /// `Arc` held past this guard's drop still reads its own consistent view.
    pub fn tier_arc(&self) -> std::sync::Arc<crate::memory_tier::TierSnapshot> {
        self.tier.arc()
    }

    /// SPEC-24 S6 as-of token: the commit version this view is pinned to (==
    /// the engine's logical clock, ADR-0018).
    pub fn logical_time(&self) -> u64 {
        self.tier.version()
    }

    /// True if `(g, s, p, o)` is visible at this pinned version — the
    /// graph-scoped point read (SPEC-24 S6, SPEC-28 S2). O(log rows in the
    /// predicate partition): see [`PredicatePartition::contains_at`]. An
    /// absent graph or predicate is `false`, not an error.
    ///
    /// One caveat, inherited from HDB-84: the first read of a partition that
    /// a batched write left as several runs merges them, which is O(rows in
    /// that partition) and happens once. A point read right after a bulk load
    /// can therefore pay that merge; every later one is the binary search.
    pub fn contains_quad(&self, g: GraphId, s: TermId, p: TermId, o: TermId) -> bool {
        let version = self.tier.version();
        self.tier
            .with_predicate(g, p, |part| part.contains_at(s, o, version))
            .unwrap_or(false)
    }

    // --- default-graph-scoped ---

    /// True if `(s, p, o)` is visible in the default graph at this pinned
    /// version. The default-graph alias of [`Self::contains_quad`].
    pub fn contains(&self, s: TermId, p: TermId, o: TermId) -> bool {
        self.contains_quad(DEFAULT_GRAPH, s, p, o)
    }

    /// Key-ordered iteration over every visible default-graph triple as raw
    /// `TermId`s: predicates in ascending id order, subject-major within each
    /// predicate. Stable across concurrent writes (reads the pinned view).
    pub fn iter_all_term_ids(&self) -> impl Iterator<Item = (TermId, TermId, TermId)> + '_ {
        self.iter_graph_term_ids(DEFAULT_GRAPH)
    }

    /// Dump every default-graph triple as raw `TermId`s, key-ordered
    /// (predicates ascending, subject-major within each predicate — see
    /// [`Self::iter_all_term_ids`]), from this single pinned snapshot (so the
    /// dump is internally consistent even under concurrent writes — the NF5
    /// checkpoint-consistency property).
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        let mut out = Vec::with_capacity(self.graph_len(DEFAULT_GRAPH));
        out.extend(self.iter_all_term_ids());
        out
    }

    /// Scan a single predicate in the default graph in the requested index
    /// ordering (SPEC-02 F4), returning materialized `(s, p, o)` triples.
    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        let p_id = match self.dictionary.get(predicate) {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let cols = match self.tier.ordered_predicate_at(DEFAULT_GRAPH, p_id, ord) {
            Some(cols) => cols,
            None => return Ok(Vec::new()),
        };
        let mut out = Vec::with_capacity(cols.len());
        for (s_id, o_id) in cols.subject_object() {
            out.push((self.term(s_id)?, predicate.clone(), self.term(o_id)?));
        }
        Ok(out)
    }

    /// The top-`n` predicates in the default graph by triple count (descending).
    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        let top = self.tier.top_predicates(DEFAULT_GRAPH, n);
        let mut out = Vec::with_capacity(top.len());
        for (p_id, count) in top {
            out.push((self.term(p_id)?, count));
        }
        Ok(out)
    }

    // --- graph-scoped (SPEC-28 S2) ---

    /// Number of triples visible in graph `g` at this pinned view. O(number
    /// of predicates in `g`); unconditional — an absent or never-interned
    /// graph yields 0, not an error.
    pub fn graph_len(&self, g: GraphId) -> usize {
        self.tier.graph_len(g)
    }

    /// The graphs holding at least one visible quad in this pinned view (D11
    /// — see [`crate::tier::Tier::graphs`] for the full contract), including
    /// `DEFAULT_GRAPH` when it holds data. Callers wanting named graphs only
    /// should filter out `DEFAULT_GRAPH`. Sorted by `GraphId` for a
    /// deterministic result — public API that Graph Store Protocol responses
    /// enumerate.
    pub fn graphs(&self) -> Vec<GraphId> {
        let mut graphs = self.tier.graphs();
        graphs.sort_by_key(|g| g.0);
        graphs
    }

    /// Decode `g` back to the IRI [`Term`] it was interned from. Errors on
    /// `DEFAULT_GRAPH`: the sentinel has no dictionary entry — it is not a
    /// real graph name.
    pub fn graph_uri(&self, g: GraphId) -> Result<Term> {
        if g == DEFAULT_GRAPH {
            return Err(crate::StorageError::InvalidTerm(
                "the default graph has no IRI (it is a sentinel, not a named graph)".into(),
            ));
        }
        self.term(TermId(g.0))
    }

    /// Every visible triple in graph `g`, decoded, from this single pinned
    /// snapshot. O(quads in `g` + predicates in `g`) — never O(store). This is
    /// the GSP (Graph Store Protocol) `GET` path and stage 3 of the
    /// whole-graph `PUT` diff (SPEC-28), so it must stay graph-scoped rather
    /// than filtering a whole-store scan. Predicates ascending, subject-major
    /// within each predicate.
    pub fn scan_graph(&self, g: GraphId) -> Result<Vec<(Term, Term, Term)>> {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(g);
        preds.sort_by_key(|t| t.0);
        let mut out = Vec::with_capacity(self.graph_len(g));
        for p_id in preds {
            let p = self.term(p_id)?;
            self.tier
                .with_predicate(g, p_id, |part| -> Result<()> {
                    for (s_id, o_id) in part.scan_at(version) {
                        out.push((self.term(s_id)?, p.clone(), self.term(o_id)?));
                    }
                    Ok(())
                })
                .transpose()?;
        }
        Ok(out)
    }

    /// Key-ordered iteration over every visible triple in graph `g`, as raw
    /// `TermId`s: predicates in ascending id order, subject-major within each
    /// predicate. The id-level twin of [`Self::scan_graph`] (SPEC-28).
    pub fn iter_graph_term_ids(
        &self,
        g: GraphId,
    ) -> impl Iterator<Item = (TermId, TermId, TermId)> + '_ {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(g);
        preds.sort_by_key(|t| t.0);
        preds.into_iter().flat_map(move |p_id| {
            self.tier
                .with_predicate(g, p_id, |part| {
                    part.scan_at(version)
                        .map(move |(s, o)| (s, p_id, o))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    }

    /// Scan a single predicate in `g`, returning materialized (subject,
    /// object) `Term` pairs, subject-major. A read transaction never mutates
    /// the dictionary: an absent predicate (never interned) yields no rows.
    pub fn scan_predicate(&self, g: GraphId, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        let p_id = match self.dictionary.get(predicate) {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let pairs = self
            .tier
            .with_predicate(g, p_id, |part| {
                part.scan_at(self.tier.version()).collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut out = Vec::with_capacity(pairs.len());
        for (s_id, o_id) in pairs {
            out.push((self.term(s_id)?, self.term(o_id)?));
        }
        Ok(out)
    }

    /// True if any non-default graph in this pinned snapshot holds at least
    /// one triple; lets an exporter check this and scan the default graph
    /// from the same snapshot (no TOCTOU between the check and the scan).
    pub fn has_named_graph_data(&self) -> bool {
        self.tier.graphs().into_iter().any(|g| g != DEFAULT_GRAPH)
    }

    /// The append-only dictionary backing this snapshot, for term materialization.
    pub fn dictionary(&self) -> &Dictionary {
        self.dictionary
    }

    fn term(&self, id: TermId) -> Result<Term> {
        self.dictionary
            .lookup(id)
            .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {id:?}")))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
