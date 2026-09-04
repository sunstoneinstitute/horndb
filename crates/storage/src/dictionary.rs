//! Concurrent term↔ID dictionary.
//!
//! Forward map: `DashMap<Box<[u8]>, TermId>` (lock-free reads, sharded writes),
//! keyed on a compact byte encoding of the term rather than on the
//! `oxrdf::Term` itself — see [`encode_key`].
//! Reverse map: `RwLock<Vec<Option<Term>>>` indexed by `payload - 1`; a
//! `None` slot is a term [`Dictionary::gc`] reclaimed.
//!
//! ## The persistent base (SPEC-25 S2)
//!
//! A dictionary opened with [`Dictionary::open`] sits on an immutable,
//! memory-mapped **base** ([`crate::dict_base`]) holding every index a
//! previous process flushed: indices `1..=base_len` resolve through the base,
//! and the two maps above — the **overlay** — hold only what this process
//! interned, numbered from `base_len + 1`. So a fresh dictionary has
//! `base_len == 0` and runs exactly the code it ran before the base existed,
//! and a reopened one issues the id a term would have got without the
//! restart. [`Dictionary::flush`] writes base + overlay as one new base file.
//!
//! Probe order is overlay first, then base, and that order is load-bearing: a
//! base term [`Dictionary::gc`] reclaimed and a later `intern` re-issued lives
//! in the overlay under a new id, while the base FST still maps its bytes to
//! the dead one. The base keeps a set of indices reclaimed since it was
//! written (`Overlay::base_dead`) so a dead base id resolves to nothing in
//! either direction; the next flush writes them as tombstones.
//!
//! ## Append-only, with a sweep (HDB-121)
//!
//! Interning only appends, and an index, once handed out, keeps its meaning
//! for the life of the process. [`Dictionary::gc`] — driven from
//! [`crate::Store::compact`], the same trigger as row compaction — reclaims
//! the *bytes* of terms no reachable row mentions (the reverse `Term` and the
//! forward-map key) but leaves the index consumed: nothing hands a freed index
//! back out. So id stability is unchanged, and a `Some(id)` from
//! [`Dictionary::get`] still names the same term forever.
//!
//! ## Why the forward map is keyed on bytes (HDB-95)
//!
//! A typed literal's key used to be the whole term: lexical form **plus** the
//! datatype IRI. On DBpedia infobox-properties EN that averaged 47.5 B of key
//! over 8.2 B of lexical form — about 80% of the bytes hashed, compared and
//! stored for that column were a datatype IRI repeated across millions of
//! terms drawn from a set of a few dozen. The same holds, less dramatically,
//! for the language tag of a language-tagged literal.
//!
//! The key now carries a small dense id for the datatype IRI / language tag
//! instead of its text. The ids come from [`AuxTable`], a side table private
//! to the dictionary; they are **not** `TermId`s (see the note on `AuxTable`).
//!
//! ## Why this cannot merge terms that differ lexically
//!
//! RDF term equality is lexical: `"42"`, `"042"` and `"+42"` are three terms.
//! Two guarantees keep them apart:
//!
//! 1. [`encode_key`] is injective. Every field is either fixed-width,
//!    length-prefixed, or the last field of its encoding (so it consumes the
//!    rest of the buffer). The lexical form is copied byte-for-byte and is
//!    never canonicalised, parsed or normalised.
//! 2. An `AuxTable` id maps to exactly one string and never changes, so
//!    substituting the id for the text is a bijection on the datatype /
//!    language field.
//!
//! Together: two terms share a key if and only if they are the same term.
//! Nothing decodes a key — [`Dictionary::lookup`] returns the `Term` stored in
//! the reverse vector, so the round trip is a clone of the original term.
//!
//! The one carve-out that *does* look at the lexical form is `try_inline_int`,
//! and it inlines only the canonical form of an `i32` for exactly this reason.
//!
//! ## Not a persistence format — do not write these keys to disk
//!
//! This encoding is valid only next to the `AuxTable`s that produced it, and
//! only for the lifetime of this process. **`AuxTable` ids are assigned in
//! first-seen order**, so the same corpus interned in a different order — a
//! reimport, a different file order, a different number of loader threads
//! reaching the first typed literal — produces *different key bytes for the
//! same term*. Nothing detects that: a key written out under one ordering and
//! read back under another silently resolves to the wrong term or to no term.
//!
//! So: never persist a key, never ship one between processes, and never derive
//! an on-disk structure (front-coded block, FST, MPHF, checkpoint) from these
//! bytes without first mapping them back through the term they came from. The
//! durable encoding is [`crate::snapshot::term_codec`], which spells the
//! datatype IRI out and is self-contained by design. SPEC-25 S2's mapped
//! dictionary base must build on that one, not on this.

use crate::dict_base::{BaseStats, MappedBase};
use crate::error::{Result, StorageError};
use crate::snapshot::term_codec;
use crate::term::{GraphId, InternedQuad, TermId, TermKind, KIND_SHIFT, MAX_DICT_INDEX};
use dashmap::DashMap;
use oxrdf::{Literal, NamedNodeRef, Term};
use parking_lot::RwLock;
use roaring::RoaringTreemap;
use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

// Key tags. Values match `snapshot::term_codec` for readability; the two
// encodings are independent (a snapshot must be self-contained, so it spells
// the datatype IRI out, while this one substitutes an `AuxTable` id).
const K_URI: u8 = 0x00;
const K_BLANK: u8 = 0x01;
const K_PLAIN: u8 = 0x02;
const K_LANG: u8 = 0x03;
const K_TYPED: u8 = 0x04;
const K_TRIPLE: u8 = 0x06;
const K_DIR_LANG: u8 = 0x07;

/// Distinguishes one `Dictionary` from another for the thread-local aux memo.
static NEXT_DICT_ID: AtomicU64 = AtomicU64::new(0);

/// A dense id for one of the short, highly repetitive strings that qualify a
/// literal: a datatype IRI or a language tag. A few dozen per corpus, so the
/// id almost always varint-encodes to a single byte.
///
/// These are **not** `TermId`s and share no numbering with them. Reusing the
/// main dictionary would have meant interning every datatype IRI as a
/// `TermKind::Uri` term, which changes the `TermId` a document's terms get
/// (datatype IRIs are rarely terms in their own right) and inflates
/// `Dictionary::len()` with entries no triple refers to. A side table keeps
/// `TermId` assignment byte-identical to before this change.
struct AuxTable {
    map: DashMap<Box<str>, u32>,
    /// First-seen order, and the allocation lock. Ids are assigned in the
    /// order strings are first seen, so a document interned twice produces
    /// the same table.
    order: RwLock<Vec<Box<str>>>,
}

impl AuxTable {
    fn new() -> Self {
        Self {
            map: DashMap::new(),
            order: RwLock::new(Vec::new()),
        }
    }

    fn get(&self, s: &str) -> Option<u32> {
        self.map.get(s).map(|e| *e)
    }

    fn intern(&self, s: &str) -> u32 {
        if let Some(id) = self.get(s) {
            return id;
        }
        let mut order = self.order.write();
        if let Some(id) = self.get(s) {
            return id;
        }
        let id = order.len() as u32;
        order.push(s.into());
        self.map.insert(s.into(), id);
        id
    }
}

/// One-entry-per-role memo of the last `AuxTable` lookup this thread made.
///
/// Without it the datatype IRI would still be hashed on every intern, just in
/// a smaller map — so the change would shrink stored and compared bytes but
/// not hashed bytes. Datatype IRIs and language tags arrive in long runs, so a
/// single entry answers nearly every call with a length check and a `memcmp`.
///
/// Sound because an `AuxTable` id, once assigned, never changes. Keyed on the
/// owning dictionary's id so a memo entry cannot leak between two
/// `Dictionary` instances on the same thread.
#[derive(Default)]
struct AuxMemo {
    dict_id: u64,
    text: String,
    id: u32,
    valid: bool,
}

impl AuxMemo {
    #[inline]
    fn get(&self, dict_id: u64, s: &str) -> Option<u32> {
        if self.valid && self.dict_id == dict_id && self.text == s {
            Some(self.id)
        } else {
            None
        }
    }

    fn put(&mut self, dict_id: u64, s: &str, id: u32) {
        self.dict_id = dict_id;
        self.text.clear();
        self.text.push_str(s);
        self.id = id;
        self.valid = true;
    }
}

#[derive(Default)]
struct KeyScratch {
    buf: Vec<u8>,
    /// `term_codec` bytes for the base probe, so a base lookup allocates
    /// nothing either.
    codec: Vec<u8>,
    datatype: AuxMemo,
    language: AuxMemo,
}

thread_local! {
    static SCRATCH: RefCell<KeyScratch> = RefCell::new(KeyScratch::default());
}

/// Which of the two side tables an aux id comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Aux {
    Datatype,
    Language,
}

/// Whether a missing aux id should be created (interning) or reported as
/// "this term cannot be in the dictionary" (read-only lookup).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuxMode {
    Intern,
    Lookup,
}

/// Sub-phase split of [`Dictionary::intern`] (HDB-106).
///
/// `intern` is the largest phase of a bulk load, and "3 seconds of interning"
/// says nothing about *which* part of interning. This splits it into the four
/// pieces that can be attacked separately:
///
/// | phase | what it covers |
/// |---|---|
/// | `intern_encode` | [`Dictionary::encode_key`] — building the byte key |
/// | `intern_probe` | the `forward` lookup that answers a hit |
/// | `intern_miss` | the whole slow path a failed probe falls into |
/// | `intern_reverse` | the reverse-map write **inside** `intern_miss` |
///
/// `intern_encode + intern_probe` is the hit path; a miss pays those plus
/// `intern_miss`, of which `intern_reverse` is a part. So the four do not sum
/// to `intern` — two of them nest.
///
/// **Off unless `HORNDB_INTERN_PHASES=1`.** Interning a term costs on the
/// order of 100 ns and this takes two `Instant::now()` pairs per call (HDB-90
/// measured a pair at 16.5 ns), so leaving it on would inflate the very phase
/// it is attributing by roughly a third. SPEC-17 §5.3 forbids per-tuple
/// instruments on the shipped path; this is a diagnostic you turn on for one
/// run, exactly like `HORNDB_EXEC_PHASES=1`.
///
/// The accumulator is thread-local and merged into the process counters by
/// [`flush_intern_phases`], once at the end of a load — never per term.
/// **The bulk loaders (`QuadSink::finish`) are its only caller**, so the split
/// covers the storage bulk-load path and nothing else: interning done through
/// `Store::apply_quads` or `HornBackend`'s dedupe loop accumulates on those
/// threads and is simply never published. Those paths have their own phases
/// (`intern`, `dedupe`); extending the split to them is a separate change.
mod intern_phases {
    use horndb_metrics::labels::LoadPhase;
    use std::cell::RefCell;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    pub(super) const ENCODE: usize = 0;
    pub(super) const PROBE: usize = 1;
    pub(super) const MISS: usize = 2;
    pub(super) const REVERSE: usize = 3;

    const ORDER: [LoadPhase; 4] = [
        LoadPhase::InternEncode,
        LoadPhase::InternProbe,
        LoadPhase::InternMiss,
        LoadPhase::InternReverse,
    ];

    thread_local! {
        /// Per phase: (nanoseconds, calls).
        static ACC: RefCell<[(u64, u64); 4]> = const { RefCell::new([(0, 0); 4]) };
    }

    /// Read once per process. A `Dictionary` copies it into a field at
    /// construction so the shipped path pays a predictable branch on a cache-hot
    /// bool, not an atomic load, per intern call.
    pub(super) fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var("HORNDB_INTERN_PHASES").as_deref() == Ok("1"))
    }

    /// Clock reading, at the call sites that are only reached with the split
    /// on. `Option` so [`charge`] has one shape for both.
    #[inline]
    pub(super) fn start() -> Option<Instant> {
        Some(Instant::now())
    }

    #[inline]
    pub(super) fn charge(phase: usize, since: Option<Instant>) {
        let Some(t) = since else { return };
        let ns = t.elapsed().as_nanos() as u64;
        ACC.with(|a| {
            let mut a = a.borrow_mut();
            a[phase].0 += ns;
            a[phase].1 += 1;
        });
    }

    /// Merge this thread's accumulator into the process counters and reset it.
    /// A no-op when the split is off, so callers need no gate of their own.
    pub(super) fn flush() {
        ACC.with(|a| {
            for (i, (ns, calls)) in a.replace([(0, 0); 4]).into_iter().enumerate() {
                if ns == 0 && calls == 0 {
                    continue;
                }
                horndb_metrics::metrics().storage.record_load_phase(
                    ORDER[i].clone(),
                    Duration::from_nanos(ns),
                    calls,
                );
            }
        });
    }
}

/// Publish this thread's [`intern_phases`] accumulator to the process
/// counters. Called once at the end of a load, never per term; a no-op unless
/// `HORNDB_INTERN_PHASES=1`.
///
/// Called from `QuadSink::finish` only. A thread that interns without going
/// through a bulk loader accumulates sub-phase time that nothing publishes —
/// see [`intern_phases`].
pub fn flush_intern_phases() {
    intern_phases::flush();
}

/// Everything the reverse lock guards.
struct Overlay {
    /// Reverse map of the overlay, indexed by `payload - 1 - base_len`. A
    /// `None` slot is a term that [`Dictionary::gc`] reclaimed: its lexical
    /// bytes and its forward-map key are gone, and the index is free. **Free
    /// indices are not re-issued** — see [`Dictionary::gc`].
    terms: Vec<Option<Term>>,
    /// Base indices [`Dictionary::gc`] reclaimed since the base was written.
    /// The base file is immutable, so this is the only record until the next
    /// flush tombstones them.
    base_dead: RoaringTreemap,
}

pub struct Dictionary {
    id: u64,
    forward: DashMap<Box<[u8]>, TermId>,
    reverse: RwLock<Overlay>,
    /// How many indices resolve to nothing: base tombstones, `base_dead`, and
    /// `None` overlay slots. Cheaper than counting them, and `len()` minus
    /// this is the live-term gauge.
    freed: AtomicU64,
    /// Indices `1..=base_len` resolve through the mapped base (SPEC-25 S2).
    base: Option<MappedBase>,
    base_len: u64,
    datatypes: AuxTable,
    languages: AuxTable,
    /// `HORNDB_INTERN_PHASES=1`, resolved once at construction.
    phases: bool,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::with_base(None)
    }

    fn with_base(base: Option<MappedBase>) -> Self {
        let (base_len, freed) = base.as_ref().map_or((0, 0), |b| (b.slots(), b.freed()));
        Self {
            id: NEXT_DICT_ID.fetch_add(1, Ordering::Relaxed),
            forward: DashMap::new(),
            reverse: RwLock::new(Overlay {
                terms: Vec::new(),
                base_dead: RoaringTreemap::new(),
            }),
            freed: AtomicU64::new(freed),
            base,
            base_len,
            datatypes: AuxTable::new(),
            languages: AuxTable::new(),
            phases: intern_phases::enabled(),
        }
    }

    /// Reopen a dictionary on the base file a previous [`Dictionary::flush`]
    /// wrote. Cost is the mmap plus header validation — nothing is
    /// re-interned. Every index the file covers resolves both ways (unless it
    /// was reclaimed before the flush), and the next `intern` of a new term
    /// gets index `len() + 1`.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self::with_base(Some(MappedBase::open(path)?)))
    }

    /// Write every index this dictionary has issued — base and overlay
    /// merged, reclaimed indices as tombstones — as one new base file at
    /// `path`, with `next_bnode_doc_tag` (the store's counter, see
    /// [`Store::flush_dictionary`](crate::Store::flush_dictionary)) in the
    /// header. Atomic (unique temp file + rename + directory fsync), so
    /// `path` may be the file this dictionary is open on. Holds the reverse
    /// read lock for the duration: terms interned meanwhile are not in the
    /// file, and `intern` blocks. A damaged base slot fails the flush rather
    /// than becoming a tombstone.
    ///
    /// ponytail: the running process keeps using its old base and overlay
    /// after the flush; the merged file is only read by the next `open`.
    /// Ceiling: overlay memory is released at restart, not at checkpoint.
    /// Upgrade path: rebuild the base under the reverse write lock, then
    /// `forward.retain(|_, id| id.payload() > flushed)`.
    pub fn flush(&self, path: &Path, next_bnode_doc_tag: u64) -> Result<BaseStats> {
        let overlay = self.reverse.read();
        let base_len = self.base_len;
        let base_slots = (1..=base_len).map(|idx| {
            if overlay.base_dead.contains(idx) {
                return Ok(None);
            }
            let base = self.base.as_ref().ok_or_else(|| {
                StorageError::Snapshot(format!("dictionary base: index {idx} has no base"))
            })?;
            let Some(bytes) = base.slot(idx)? else {
                return Ok(None);
            };
            let kind = term_codec::encoded_kind(bytes).ok_or_else(|| {
                StorageError::Snapshot(format!("dictionary base: index {idx}: unknown term tag"))
            })?;
            Ok(Some((bytes.to_vec(), TermId::new(kind, idx))))
        });
        let overlay_slots = overlay.terms.iter().enumerate().map(|(i, slot)| {
            let Some(term) = slot.as_ref() else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            term_codec::encode_term(&mut bytes, term);
            Ok(Some((
                bytes,
                TermId::new(kind_of(term), base_len + i as u64 + 1),
            )))
        });
        MappedBase::write(path, base_slots.chain(overlay_slots), next_bnode_doc_tag)
    }

    /// The blank-node document tag the base was flushed with; `0` without a
    /// base. `Store::with_dictionary` seeds its counter from it.
    pub fn base_bnode_doc_tag(&self) -> u64 {
        self.base.as_ref().map_or(0, MappedBase::next_bnode_doc_tag)
    }

    /// Full integrity check of the mapped base (offset table and FST
    /// checksum) — opt-in because it reads the whole file; `open` checks
    /// only the header. `Ok` without a base.
    pub fn verify(&self) -> Result<()> {
        self.base.as_ref().map_or(Ok(()), MappedBase::verify)
    }

    /// Indices the mapped base covers; `0` for a dictionary with no base.
    pub fn base_len(&self) -> usize {
        self.base_len as usize
    }

    /// Indices this dictionary has ever handed out, base included. Monotonic:
    /// it does not drop when [`Dictionary::gc`] reclaims terms, because a
    /// reclaimed index is never re-issued. Use [`Dictionary::live_len`] for
    /// the number of terms currently resolvable.
    pub fn len(&self) -> usize {
        self.base_len as usize + self.reverse.read().terms.len()
    }

    /// Terms currently resolvable — [`Dictionary::len`] minus the slots
    /// [`Dictionary::gc`] has reclaimed. Equal to `len()` until the first GC.
    pub fn live_len(&self) -> usize {
        self.len() - self.freed.load(Ordering::Relaxed) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn table(&self, aux: Aux) -> &AuxTable {
        match aux {
            Aux::Datatype => &self.datatypes,
            Aux::Language => &self.languages,
        }
    }

    /// Resolve one aux string to its dense id, consulting (and refreshing) the
    /// thread-local memo. `None` only in [`AuxMode::Lookup`] and only when the
    /// string has never been seen.
    fn aux_id(&self, scratch: &mut KeyScratch, aux: Aux, s: &str, mode: AuxMode) -> Option<u32> {
        let memo = match aux {
            Aux::Datatype => &mut scratch.datatype,
            Aux::Language => &mut scratch.language,
        };
        if let Some(id) = memo.get(self.id, s) {
            return Some(id);
        }
        let table = self.table(aux);
        let id = match mode {
            AuxMode::Intern => table.intern(s),
            AuxMode::Lookup => table.get(s)?,
        };
        let memo = match aux {
            Aux::Datatype => &mut scratch.datatype,
            Aux::Language => &mut scratch.language,
        };
        memo.put(self.id, s, id);
        Some(id)
    }

    /// Encode `term` into `scratch.buf` (cleared first). Returns `false` only
    /// in [`AuxMode::Lookup`], when a datatype IRI or language tag the term
    /// carries has never been interned — which proves the term itself has
    /// never been interned either.
    ///
    /// **A `false` return leaves `scratch.buf` partially built.** Inserting
    /// that truncated buffer as a key would let two different terms share one
    /// key, so every caller must check the result — hence `#[must_use]`.
    #[must_use]
    fn encode_key(&self, scratch: &mut KeyScratch, term: &Term, mode: AuxMode) -> bool {
        scratch.buf.clear();
        self.encode_into(scratch, term, mode)
    }

    fn encode_into(&self, scratch: &mut KeyScratch, term: &Term, mode: AuxMode) -> bool {
        match term {
            Term::NamedNode(n) => {
                scratch.buf.push(K_URI);
                scratch.buf.extend_from_slice(n.as_str().as_bytes());
            }
            Term::BlankNode(b) => {
                scratch.buf.push(K_BLANK);
                scratch.buf.extend_from_slice(b.as_str().as_bytes());
            }
            Term::Literal(lit) => {
                // Direction first: an RDF 1.2 directional literal reports both
                // a language and a base direction, so testing `language()`
                // first would silently drop the direction from the key.
                if let Some(dir) = lit.direction() {
                    let lang = lit
                        .language()
                        .expect("directional language literal always has a language tag");
                    let Some(lang_id) = self.aux_id(scratch, Aux::Language, lang, mode) else {
                        return false;
                    };
                    scratch.buf.push(K_DIR_LANG);
                    scratch.buf.push(match dir {
                        oxrdf::BaseDirection::Ltr => 0u8,
                        oxrdf::BaseDirection::Rtl => 1u8,
                    });
                    push_uvarint(&mut scratch.buf, lang_id);
                    scratch.buf.extend_from_slice(lit.value().as_bytes());
                } else if let Some(lang) = lit.language() {
                    let Some(lang_id) = self.aux_id(scratch, Aux::Language, lang, mode) else {
                        return false;
                    };
                    scratch.buf.push(K_LANG);
                    push_uvarint(&mut scratch.buf, lang_id);
                    scratch.buf.extend_from_slice(lit.value().as_bytes());
                } else if lit.datatype().as_str() == XSD_STRING {
                    scratch.buf.push(K_PLAIN);
                    scratch.buf.extend_from_slice(lit.value().as_bytes());
                } else {
                    let dt = lit.datatype();
                    let Some(dt_id) = self.aux_id(scratch, Aux::Datatype, dt.as_str(), mode) else {
                        return false;
                    };
                    scratch.buf.push(K_TYPED);
                    push_uvarint(&mut scratch.buf, dt_id);
                    scratch.buf.extend_from_slice(lit.value().as_bytes());
                }
            }
            Term::Triple(t) => {
                // Subterms are length-prefixed so the variable-length kinds,
                // which otherwise run to the end of the buffer, stay
                // unambiguous when nested.
                scratch.buf.push(K_TRIPLE);
                // A subterm needs its own buffer so its length can be written
                // before its bytes, and it cannot borrow the thread-local one
                // — that is already borrowed by the call in flight. Allocating
                // here does not cost the hit path anything: every non-triple
                // kind returns above, and triple terms are an RDF 1.2 rarity.
                let mut sub = KeyScratch::default();
                let s: Term = match &t.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                    oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
                };
                let p = Term::NamedNode(t.predicate.clone());
                for child in [&s, &p] {
                    if !self.encode_key(&mut sub, child, mode) {
                        return false;
                    }
                    push_uvarint(&mut scratch.buf, sub.buf.len() as u32);
                    scratch.buf.extend_from_slice(&sub.buf);
                }
                // The object is last, so it needs no length prefix.
                if !self.encode_into(scratch, &t.object, mode) {
                    return false;
                }
            }
        }
        true
    }

    pub fn intern(&self, term: &Term) -> Result<TermId> {
        // Inline-int fast path.
        if let Some(id) = try_inline_int(term) {
            return Ok(id);
        }
        SCRATCH.with(|s| {
            let mut scratch = s.borrow_mut();
            // `AuxMode::Intern` creates any missing aux id rather than
            // reporting it, so this cannot fail today. Asserted rather than
            // assumed: on a `false` return `scratch.buf` holds a truncated
            // key, and inserting that below would alias two distinct terms.
            // One branch, taken once per call, rather than a gate at each of
            // the three timing points: `Option<Instant>` is lazy but not free,
            // and four extra branches on a ~100 ns hot path cost ~1.7% of a
            // one-thread load. Both arms call the same `encode_key` /
            // `forward.get` / `intern_miss`, so the timed copy cannot drift
            // from the shipped one on anything that decides a term id.
            if self.phases {
                return self.intern_timed(&mut scratch, term);
            }
            let encoded = self.encode_key(&mut scratch, term, AuxMode::Intern);
            debug_assert!(encoded, "encode_key must not fail in AuxMode::Intern");
            if !encoded {
                return Err(StorageError::InvalidTerm(format!(
                    "term key could not be encoded: {term}"
                )));
            }
            if let Some(existing) = self.forward.get(scratch.buf.as_slice()) {
                return Ok(*existing);
            }
            if let Some(existing) = self.base_get(&mut scratch, term) {
                return Ok(existing);
            }
            self.intern_miss(&scratch, term)
        })
    }

    /// The base half of a term → id probe: `None` with no base, for a term
    /// the base does not hold, or for one it holds under an index
    /// [`Dictionary::gc`] has since reclaimed. Only reached on an overlay
    /// miss, so a dictionary without a base never pays for it on a hit.
    fn base_get(&self, scratch: &mut KeyScratch, term: &Term) -> Option<TermId> {
        let base = self.base.as_ref()?;
        scratch.codec.clear();
        term_codec::encode_term(&mut scratch.codec, term);
        let id = base.get(&scratch.codec)?;
        let overlay = self.reverse.read();
        (!overlay.base_dead.contains(id.payload())).then_some(id)
    }

    /// [`Dictionary::intern`] with the [`intern_phases`] split taken. Reached
    /// only under `HORNDB_INTERN_PHASES=1`.
    fn intern_timed(&self, scratch: &mut KeyScratch, term: &Term) -> Result<TermId> {
        let t_encode = intern_phases::start();
        let encoded = self.encode_key(scratch, term, AuxMode::Intern);
        intern_phases::charge(intern_phases::ENCODE, t_encode);
        debug_assert!(encoded, "encode_key must not fail in AuxMode::Intern");
        if !encoded {
            return Err(StorageError::InvalidTerm(format!(
                "term key could not be encoded: {term}"
            )));
        }
        let t_probe = intern_phases::start();
        let hit = self
            .forward
            .get(scratch.buf.as_slice())
            .map(|e| *e)
            .or_else(|| self.base_get(scratch, term));
        intern_phases::charge(intern_phases::PROBE, t_probe);
        if let Some(existing) = hit {
            return Ok(existing);
        }
        let t_miss = intern_phases::start();
        let out = self.intern_miss(scratch, term);
        intern_phases::charge(intern_phases::MISS, t_miss);
        out
    }

    /// The slow path of [`Dictionary::intern`]: the probe found nothing, so
    /// take the reverse-vector write lock, re-check under it, and append.
    ///
    /// Split out so the sub-phase split can time it as a unit without an
    /// early-return in the middle, and so the hit path is a short function the
    /// optimiser can keep tight.
    fn intern_miss(&self, scratch: &KeyScratch, term: &Term) -> Result<TermId> {
        let mut overlay = self.reverse.write();
        if let Some(existing) = self.forward.get(scratch.buf.as_slice()) {
            return Ok(*existing);
        }
        let next_index = self.base_len + (overlay.terms.len() as u64) + 1;
        if next_index >= MAX_DICT_INDEX {
            return Err(StorageError::DictionaryFull(next_index));
        }
        let kind = kind_of(term);
        let id = TermId::new(kind, next_index);
        // Gated inside the miss path, which is ~6% of intern calls, so the
        // branch here does not need hoisting the way the hit path's did.
        let t_rev = self.phases.then(intern_phases::start).flatten();
        overlay.terms.push(Some(term.clone()));
        intern_phases::charge(intern_phases::REVERSE, t_rev);
        self.forward.insert(scratch.buf.as_slice().into(), id);
        Ok(id)
    }

    /// Intern a subject/predicate/object triple in one call, returning their
    /// `TermId`s. Convenience over three [`Dictionary::intern`] calls, shared by
    /// the bulk loaders and [`crate::Store`]'s insert paths.
    pub fn intern_triple(&self, s: &Term, p: &Term, o: &Term) -> Result<(TermId, TermId, TermId)> {
        Ok((self.intern(s)?, self.intern(p)?, self.intern(o)?))
    }

    /// Intern a quad's subject/predicate/object against an already-resolved
    /// `GraphId`, returning the [`InternedQuad`] the id-based store entry
    /// points require. Interns in subject, predicate, object order — the same
    /// order, and the same one-intern-per-new-term, as
    /// [`Dictionary::intern_triple`], so a document interned through this path
    /// gets exactly the ids the term-based path would have assigned.
    pub fn intern_quad(&self, g: GraphId, s: &Term, p: &Term, o: &Term) -> Result<InternedQuad> {
        let (s_id, p_id, o_id) = self.intern_triple(s, p, o)?;
        Ok(InternedQuad::from_ids(g, s_id, p_id, o_id))
    }

    /// Pair a graph id with three term ids **this dictionary already
    /// issued** into the [`InternedQuad`] the id-based store entry points
    /// take. The `&self` receiver is the guard: the only way to hold ids
    /// worth pairing is to have interned them here.
    ///
    /// For callers that carry their own id-level triples and intern each
    /// distinct term once up front — the reasoner closure path (HDB-117) —
    /// instead of re-interning three terms per triple via
    /// [`Dictionary::intern_quad`].
    pub fn quad_from_ids(&self, g: GraphId, s: TermId, p: TermId, o: TermId) -> InternedQuad {
        debug_assert!(
            self.issued(s) && self.issued(p) && self.issued(o),
            "quad_from_ids called with ids this dictionary never issued"
        );
        InternedQuad::from_ids(g, s, p, o)
    }

    /// True if this dictionary could have issued `id`: an inline-int id (value
    /// encoded, never allocated) or an index it has actually handed out. The
    /// dictionary never re-issues an index, so an id it issued stays issued
    /// (a GC-reclaimed index included — it resolves to no term, but it is
    /// never handed to a different one). Backs the
    /// `debug_assert!` on the id-based store entry points; not a security
    /// boundary — a foreign dictionary of the same size passes.
    pub(crate) fn issued(&self, id: TermId) -> bool {
        match TermKind::from_tag((id.bits() >> KIND_SHIFT) as u8) {
            None => false,
            Some(TermKind::InlineInt) => true,
            Some(_) => (1..=self.len() as u64).contains(&id.payload()),
        }
    }

    /// Resolve a term to its `TermId` **without** interning it. Returns
    /// `None` if the term has never been interned (inline-int literals
    /// always resolve — they are value-encoded, not dictionary-allocated).
    /// Used by query frontends to look up constants: an absent constant
    /// means no stored triple can match it.
    ///
    /// **Safe to call concurrently with `intern` on another thread**, which is
    /// what the bulk loaders' parse-thread probe does (HDB-106). Two
    /// properties make the answer usable even though it races:
    ///
    /// * the dictionary is append-only, so a `Some(id)` stays correct — the
    ///   id a later `intern` of the same term would return is the same one;
    /// * a `None` is only ever *stale*, never wrong, because `intern` publishes
    ///   the reverse-vector entry before the forward-map key. A caller that
    ///   falls back to `intern` on `None` therefore gets exactly the id it
    ///   would have got without the probe.
    ///
    /// It also creates no `AuxTable` ids ([`AuxMode::Lookup`]): a datatype IRI
    /// or language tag first seen on a parse thread is still assigned its id
    /// by whichever thread interns the term, in that thread's order.
    pub fn get(&self, term: &Term) -> Option<TermId> {
        if let Some(id) = try_inline_int(term) {
            return Some(id);
        }
        SCRATCH.with(|s| {
            let mut scratch = s.borrow_mut();
            // An unseen datatype IRI or language tag proves the term is not
            // in the overlay; the base may still hold it.
            if self.encode_key(&mut scratch, term, AuxMode::Lookup) {
                if let Some(id) = self.forward.get(scratch.buf.as_slice()) {
                    return Some(*id);
                }
            }
            self.base_get(&mut scratch, term)
        })
    }

    /// id → term for a dictionary index (never an inline int), under the
    /// caller's read lock.
    fn resolve(&self, overlay: &Overlay, idx: u64) -> Option<Term> {
        if idx == 0 {
            None
        } else if idx <= self.base_len {
            if overlay.base_dead.contains(idx) {
                return None;
            }
            let bytes = self.base.as_ref()?.term_bytes(idx)?;
            term_codec::decode_term(bytes).ok()
        } else {
            overlay
                .terms
                .get((idx - 1 - self.base_len) as usize)
                .cloned()
                .flatten()
        }
    }

    pub fn lookup(&self, id: TermId) -> Option<Term> {
        if id.kind() == TermKind::InlineInt {
            let v = id.as_inline_int().unwrap();
            return Some(Term::Literal(Literal::new_typed_literal(
                v.to_string(),
                NamedNodeRef::new(XSD_INTEGER).unwrap(),
            )));
        }
        let overlay = self.reverse.read();
        self.resolve(&overlay, id.payload())
    }

    /// Bulk-decode a batch of **inline-int** `TermId`s to `xsd:integer`
    /// literals. Non-inline ids decode to `None`. The i32 payloads are
    /// extracted by [`Dictionary::decode_inline_ints`] (the SIMD-friendly
    /// data-parallel core), then materialised to `Term::Literal`. SPEC-12 F2
    /// / acceptance #4.
    ///
    /// The vectorisable win is in the *integer extraction* (mask the kind tag,
    /// cast the low 32 payload bits across a batch); building the
    /// `Term::Literal` strings is inherently scalar (heap allocation) and
    /// dominates only when the caller needs full `Term`s. Callers that only
    /// need the i32 values should use [`Dictionary::decode_inline_ints`], which
    /// is the path the benchmark measures.
    pub fn lookup_inline_int_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let ints = Self::decode_inline_ints(ids);
        ints.into_iter()
            .map(|opt| {
                opt.map(|v| {
                    Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    ))
                })
            })
            .collect()
    }

    /// Extract the i32 value of each inline-int `TermId` in `ids`; `None` for
    /// any id that is not `TermKind::InlineInt`. This is the data-parallel hot
    /// core (mask the kind tag, cast the low 32 payload bits) — the form the
    /// decode microbench measures for the ≥4× floor (SPEC-12 NF4).
    ///
    /// Per SPEC-12 "measure first": the loop body is a pure mask+cast unpack
    /// that the compiler autovectorises; a dedicated `horndb-simd` unpack
    /// primitive is only added if the hornbench bench shows this misses ≥4×.
    pub fn decode_inline_ints(ids: &[TermId]) -> Vec<Option<i32>> {
        // The kind tag occupies bits [60,64); inline-int tag value:
        let inline_tag = (TermKind::InlineInt as u64) << crate::term::KIND_SHIFT;
        let tag_mask = !crate::term::PAYLOAD_MASK; // top 4 bits
        ids.iter()
            .map(|&id| {
                let bits = id.bits();
                if bits & tag_mask == inline_tag {
                    Some((bits as u32) as i32)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Bulk lookup over a **mixed** batch: inline ints decode arithmetically,
    /// everything else via the reverse map under a single read lock.
    pub fn lookup_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let overlay = self.reverse.read();
        ids.iter()
            .map(|&id| {
                if id.kind() == TermKind::InlineInt {
                    let v = id.as_inline_int().unwrap();
                    Some(Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    )))
                } else {
                    self.resolve(&overlay, id.payload())
                }
            })
            .collect()
    }

    /// Read a term's numeric value directly off the stored `oxrdf::Literal`,
    /// skipping the `Term` clone + `to_string()` + N-Triples reparse that
    /// `lookup` plus the SPARQL-side numeric coercion would otherwise pay
    /// per row (HDB-100 — `eval_group_native`'s SUM/AVG/MIN/MAX fast paths).
    /// `None` for an id the dictionary never issued, a non-literal term, or
    /// a literal whose value does not parse as `f64`.
    pub fn numeric_value(&self, id: TermId) -> Option<f64> {
        if id.kind() == TermKind::InlineInt {
            return id.as_inline_int().map(|v| v as f64);
        }
        let overlay = self.reverse.read();
        match self.resolve(&overlay, id.payload())? {
            Term::Literal(lit) => lit.value().trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Sweep terms no reachable row mentions: drop their lexical bytes and
    /// their forward-map key, and mark the index free (HDB-121).
    ///
    /// `live[i]` marks dictionary index `i + 1` as reachable. Indices at or
    /// past `live.len()` are left alone, so a term interned while the caller
    /// was building the mark set is never swept. Callers build the marks from
    /// the physical rows of a tier snapshot — see [`crate::Store::compact`],
    /// the only caller; it also states the precondition on in-flight writers.
    ///
    /// Returns the number of terms freed.
    ///
    /// **A freed index is never re-issued.** The two allocations that dominate
    /// dictionary footprint are the reverse `Term` and the forward-map key;
    /// this frees both. What stays is the empty `Option<Term>` slot (a pointer
    /// pair), which is what keeps every id already handed out meaning exactly
    /// what it meant.
    ///
    /// ponytail: no id reuse. Handing an index back out is only safe once no
    /// reader can hold the old id, and nothing today establishes that: a
    /// thread that has interned a term but not yet installed its rows holds an
    /// id no snapshot version can see, so the tier's `min_pinned` bound —
    /// which is enough for reclaiming *rows* — does not cover it. Ceiling:
    /// index space is consumed at the append rate, capped at `MAX_DICT_INDEX`
    /// (2^60), not at the live-term count. Upgrade path: make id issuance
    /// visible to the reclaimer (an epoch/refcount registered at `intern` and
    /// released when the batch commits), then hand `None` slots back out here.
    /// The free set is implicit — the `None` slots themselves — so no side
    /// table has to be persisted for that; HDB-57's on-disk dictionary needs
    /// one tombstone bit per slot to carry it.
    pub fn gc(&self, live: &[bool]) -> usize {
        let mut overlay = self.reverse.write();
        let mut freed = 0u64;
        let base_len = self.base_len as usize;
        // Base slots: the file is immutable, so record the index as dead;
        // the next flush writes the tombstone. A slot the file already
        // tombstoned was counted in `freed` when the base was opened.
        for (i, marked) in live.iter().enumerate().take(base_len) {
            let idx = (i as u64) + 1;
            let held = self
                .base
                .as_ref()
                .is_some_and(|b| b.term_bytes(idx).is_some());
            if !*marked && held && overlay.base_dead.insert(idx) {
                freed += 1;
            }
        }
        SCRATCH.with(|s| {
            let mut scratch = s.borrow_mut();
            for (i, marked) in live.iter().enumerate().skip(base_len) {
                if *marked {
                    continue;
                }
                let Some(term) = overlay.terms.get_mut(i - base_len).and_then(Option::take) else {
                    continue; // past the end, or already freed
                };
                let id = TermId::new(kind_of(&term), (i as u64) + 1);
                // The aux ids this key needs already exist (the term was
                // interned), so `Lookup` cannot miss; it also keeps GC from
                // minting aux ids, which would change future key bytes.
                if self.encode_key(&mut scratch, &term, AuxMode::Lookup) {
                    self.forward
                        .remove_if(scratch.buf.as_slice(), |_, v| *v == id);
                }
                freed += 1;
            }
        });
        self.freed.fetch_add(freed, Ordering::Relaxed);
        freed as usize
    }

    /// Total bytes of forward-map key currently stored, and the number of
    /// keys. Test/benchmark instrumentation for the HDB-95 measurement; the
    /// figure excludes the `Box` headers and the map's own slot overhead.
    pub fn key_bytes(&self) -> (u64, u64) {
        let mut bytes = 0u64;
        let mut keys = 0u64;
        for e in self.forward.iter() {
            bytes += e.key().len() as u64;
            keys += 1;
        }
        (bytes, keys)
    }
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

/// LEB128-style unsigned varint. Aux ids are dense from zero and a corpus has
/// a few dozen datatypes, so this is one byte in practice.
#[inline]
fn push_uvarint(buf: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn kind_of(term: &Term) -> TermKind {
    match term {
        Term::NamedNode(_) => TermKind::Uri,
        Term::BlankNode(_) => TermKind::Blank,
        Term::Literal(lit) => {
            if lit.language().is_some() {
                TermKind::LangLiteral
            } else if lit.datatype().as_str() == XSD_STRING {
                TermKind::PlainLiteral
            } else {
                TermKind::TypedLiteral
            }
        }
        // RDF 1.2 triple terms — see SPEC-00 (vision) and TASKS.md (PR2 of
        // the RDF 1.2 migration). The key encoding recurses into the subterms,
        // so identical triple terms dedupe; the reverse `Vec<Term>` stores the
        // full `Term::Triple` recursively.
        Term::Triple(_) => TermKind::TripleTerm,
    }
}

fn try_inline_int(term: &Term) -> Option<TermId> {
    if let Term::Literal(lit) = term {
        if lit.datatype().as_str() == XSD_INTEGER {
            if let Ok(v) = lit.value().parse::<i32>() {
                // Inline only the canonical lexical form: non-canonical
                // variants ("042", "+42") must keep their own dictionary
                // identity, because RDF term equality is lexical and the
                // inline encoding can only round-trip the canonical form.
                if lit.value() == v.to_string() {
                    return Some(TermId::inline_int(v));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn typed(lex: &str, dt: &str) -> Term {
        Term::Literal(Literal::new_typed_literal(lex, NamedNode::new(dt).unwrap()))
    }

    #[test]
    fn lookup_inline_int_batch_matches_scalar() {
        let dict = Dictionary::new();
        let ids: Vec<TermId> = (-5..20).map(TermId::inline_int).collect();
        let want: Vec<Term> = ids.iter().map(|&id| dict.lookup(id).unwrap()).collect();
        let got = dict.lookup_inline_int_batch(&ids);
        assert_eq!(got.len(), ids.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.as_ref().unwrap(), w);
        }
    }

    #[test]
    fn lookup_batch_handles_mixed() {
        let dict = Dictionary::new();
        let iri = Term::NamedNode(oxrdf::NamedNode::new("http://example.org/a").unwrap());
        let iri_id = dict.intern(&iri).unwrap();
        let int_id = TermId::inline_int(42);
        let got = dict.lookup_batch(&[int_id, iri_id]);
        assert_eq!(got[0].as_ref().unwrap(), &dict.lookup(int_id).unwrap());
        assert_eq!(got[1].as_ref().unwrap(), &iri);
    }

    #[test]
    fn get_returns_id_without_interning() {
        let d = Dictionary::new();
        let t = Term::NamedNode(NamedNode::new("http://ex/a").unwrap());
        assert_eq!(d.get(&t), None);
        assert_eq!(d.len(), 0, "get must not intern");
        let id = d.intern(&t).unwrap();
        assert_eq!(d.get(&t), Some(id));
    }

    #[test]
    fn get_resolves_inline_int_without_interning() {
        let d = Dictionary::new();
        let t = Term::Literal(Literal::new_typed_literal(
            "42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let id = d.get(&t).expect("inline ints always resolve");
        assert_eq!(id, TermId::inline_int(42));
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn non_canonical_integer_keeps_distinct_identity() {
        let d = Dictionary::new();
        let canon = Term::Literal(Literal::new_typed_literal(
            "42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let padded = Term::Literal(Literal::new_typed_literal(
            "042",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let plus = Term::Literal(Literal::new_typed_literal(
            "+42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let id_canon = d.intern(&canon).unwrap();
        let id_padded = d.intern(&padded).unwrap();
        let id_plus = d.intern(&plus).unwrap();
        assert_eq!(id_canon, TermId::inline_int(42));
        assert_ne!(id_padded, id_canon);
        assert_ne!(id_plus, id_canon);
        assert_ne!(id_padded, id_plus);
        // Exact lexical round-trip for the non-canonical forms.
        assert_eq!(d.lookup(id_padded), Some(padded));
        assert_eq!(d.lookup(id_plus), Some(plus));
    }

    /// The HDB-95 guard: re-keying a typed literal on `(lexical, datatype-id)`
    /// must not merge lexically-different forms of the same value, and every
    /// form must round-trip byte-for-byte.
    #[test]
    fn typed_literal_rekeying_preserves_lexical_identity() {
        let d = Dictionary::new();
        // xsd:decimal, so `try_inline_int` is not involved — this exercises
        // the re-keyed dictionary path itself.
        const DT: &str = "http://www.w3.org/2001/XMLSchema#decimal";
        let forms = ["42", "042", "+42", "42.0"];
        let terms: Vec<Term> = forms.iter().map(|f| typed(f, DT)).collect();
        let ids: Vec<TermId> = terms.iter().map(|t| d.intern(t).unwrap()).collect();

        assert_eq!(
            d.len(),
            4,
            "four lexical forms must be four dictionary entries"
        );
        for i in 0..ids.len() {
            assert_eq!(ids[i].kind(), TermKind::TypedLiteral);
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "{} and {} collapsed", forms[i], forms[j]);
            }
        }
        for (id, (term, form)) in ids.iter().zip(terms.iter().zip(forms.iter())) {
            let back = d.lookup(*id).expect("round trip");
            assert_eq!(&back, term);
            match &back {
                Term::Literal(l) => {
                    assert_eq!(l.value(), *form, "lexical form must survive byte-for-byte");
                    assert_eq!(l.datatype().as_str(), DT);
                }
                other => panic!("expected literal, got {other:?}"),
            }
            assert_eq!(d.get(term), Some(*id), "get must find the re-keyed term");
        }

        // Same lexical forms under a different datatype are different terms.
        const DT2: &str = "http://www.w3.org/2001/XMLSchema#double";
        for (form, id) in forms.iter().zip(ids.iter()) {
            let other = typed(form, DT2);
            let other_id = d.intern(&other).unwrap();
            assert_ne!(other_id, *id, "{form} collapsed across datatypes");
            assert_eq!(d.lookup(other_id), Some(other));
        }
        assert_eq!(d.len(), 8);
    }

    /// The same guard for the language-tag substitution: a lexical form under
    /// two tags stays two terms, and the tag itself round-trips.
    #[test]
    fn lang_literal_rekeying_preserves_tag_identity() {
        let d = Dictionary::new();
        let en = Term::Literal(Literal::new_language_tagged_literal("colour", "en").unwrap());
        let en_gb = Term::Literal(Literal::new_language_tagged_literal("colour", "en-GB").unwrap());
        let plain = Term::Literal(Literal::new_simple_literal("colour"));
        let ids = [
            d.intern(&en).unwrap(),
            d.intern(&en_gb).unwrap(),
            d.intern(&plain).unwrap(),
        ];
        assert_eq!(d.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert_eq!(d.lookup(ids[0]), Some(en));
        assert_eq!(d.lookup(ids[1]), Some(en_gb));
        assert_eq!(d.lookup(ids[2]), Some(plain));
    }

    /// `get` must not create aux-table entries, and must answer `None` for a
    /// term whose datatype the dictionary has never seen.
    #[test]
    fn get_with_unseen_datatype_does_not_intern() {
        let d = Dictionary::new();
        let t = typed("x", "http://example.org/NeverSeen");
        assert_eq!(d.get(&t), None);
        assert_eq!(d.len(), 0);
        assert_eq!(
            d.datatypes.order.read().len(),
            0,
            "get must not grow the aux table"
        );
        let id = d.intern(&t).unwrap();
        assert_eq!(d.get(&t), Some(id));
    }

    /// Two dictionaries assign aux ids independently; the thread-local memo
    /// must not leak an id from one into the other's key.
    #[test]
    fn aux_memo_does_not_leak_between_dictionaries() {
        let a = Dictionary::new();
        let b = Dictionary::new();
        // `a` sees decimal first, `b` sees double first, so the same datatype
        // gets a different aux id in each.
        a.intern(&typed("1", "http://www.w3.org/2001/XMLSchema#decimal"))
            .unwrap();
        b.intern(&typed("1", "http://www.w3.org/2001/XMLSchema#double"))
            .unwrap();
        let t = typed("7", "http://www.w3.org/2001/XMLSchema#decimal");
        let a_id = a.intern(&t).unwrap();
        let b_id = b.intern(&t).unwrap();
        assert_eq!(a.lookup(a_id), Some(t.clone()));
        assert_eq!(b.lookup(b_id), Some(t.clone()));
        assert_eq!(a.get(&t), Some(a_id));
        assert_eq!(b.get(&t), Some(b_id));
        // A term only `a` has must not resolve in `b`.
        let only_a = typed("1", "http://www.w3.org/2001/XMLSchema#decimal");
        assert!(b.get(&only_a).is_none());
    }

    /// oxrdf normalises `"x"^^xsd:string` to a simple literal, so both spell
    /// the same term. The key encoding must agree with `kind_of` about that,
    /// or one of the two would get a second identity.
    #[test]
    fn xsd_string_typed_literal_is_the_plain_literal() {
        let d = Dictionary::new();
        let simple = Term::Literal(Literal::new_simple_literal("x"));
        let as_typed = typed("x", XSD_STRING);
        assert_eq!(simple, as_typed, "oxrdf must normalise xsd:string literals");
        let a = d.intern(&simple).unwrap();
        let b = d.intern(&as_typed).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.kind(), TermKind::PlainLiteral);
        assert_eq!(d.len(), 1);
    }

    /// Interning the same document twice must produce the same ids, and the
    /// aux tables must not perturb `TermId` assignment: ids stay dense from 1
    /// in first-seen order regardless of how many datatypes appear.
    #[test]
    fn ids_are_dense_and_deterministic_in_document_order() {
        let doc = || {
            vec![
                Term::NamedNode(NamedNode::new("http://ex/s").unwrap()),
                typed("1.5", "http://www.w3.org/2001/XMLSchema#decimal"),
                Term::NamedNode(NamedNode::new("http://ex/p").unwrap()),
                typed("2026-01-01", "http://www.w3.org/2001/XMLSchema#date"),
                typed("1.5", "http://www.w3.org/2001/XMLSchema#double"),
            ]
        };
        let a = Dictionary::new();
        let ids_a: Vec<TermId> = doc().iter().map(|t| a.intern(t).unwrap()).collect();
        let b = Dictionary::new();
        let ids_b: Vec<TermId> = doc().iter().map(|t| b.intern(t).unwrap()).collect();
        assert_eq!(ids_a, ids_b);
        let payloads: Vec<u64> = ids_a.iter().map(|i| i.payload()).collect();
        assert_eq!(payloads, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn key_bytes_excludes_the_datatype_iri() {
        let d = Dictionary::new();
        const DT: &str = "http://www.w3.org/2001/XMLSchema#decimal";
        d.intern(&typed("1.5", DT)).unwrap();
        let (bytes, keys) = d.key_bytes();
        assert_eq!(keys, 1);
        // tag + one-byte datatype id + "1.5"
        assert_eq!(bytes, 1 + 1 + 3);
        assert!((bytes as usize) < DT.len());
    }
}
