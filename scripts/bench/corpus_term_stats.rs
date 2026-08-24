//! corpus_term_stats — offline term-stream statistics for an N-Triples corpus.
//!
//! Answers the questions HDB-92 asks of HDB-57's R2/R3/R4: how many term
//! occurrences per distinct term, how long terms are by kind, how much of a
//! term is shared prefix, and how well a small direct-mapped repeat cache
//! keyed on `(len, first 8 bytes)` would answer dictionary calls.
//!
//! Standalone on purpose — no workspace dependency, no engine changes. The
//! `--edition 2021` is required, not decorative: a bare `rustc` defaults to
//! edition 2015, where `TryInto` is not in the prelude and `panic!("{e}")`
//! does not capture `e`. The workspace is edition 2021 throughout.
//!
//!   rustc --edition 2021 -O -o /tmp/corpus_term_stats scripts/bench/corpus_term_stats.rs
//!   /tmp/corpus_term_stats --name lubm-100 file.nt [more.nt ...]
//!   bzip2 -dc big.ttl.bz2 | /tmp/corpus_term_stats --name dbpedia -
//!
//! What counts as a "term" here is what `Dictionary::intern` keys on: an
//! `oxrdf::Term`. So the measured bytes are the *contents* — the IRI text, the
//! blank-node label, or a literal's lexical form plus its language tag or
//! datatype IRI — not the N-Triples punctuation around them. Literals whose
//! datatype is `xsd:integer` and whose lexical form is the canonical form of
//! an `i32` never reach the dictionary at all (`try_inline_int`), so they are
//! counted separately and excluded from the dictionary-facing figures.
//!
//! Output is one JSON object on stdout plus a human-readable summary on stderr.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{BufWriter, Read, Write};

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

// ---------------------------------------------------------------- hashing

/// FxHash — small, fast, good enough for counting. Not cryptographic.
#[derive(Default)]
struct FxHasher {
    hash: u64,
}
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}
impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rem.len()].copy_from_slice(rem);
            self.add(u64::from_le_bytes(buf));
        }
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}
type FxBuild = BuildHasherDefault<FxHasher>;
type FxMap<K, V> = HashMap<K, V, FxBuild>;

#[inline]
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    h.write(b);
    h.finish()
}

// ------------------------------------------------------------------ kinds

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Iri,
    Blank,
    PlainLit,
    LangLit,
    TypedLit,
    InlineInt,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Iri => "iri",
            Kind::Blank => "blank",
            Kind::PlainLit => "literal-plain",
            Kind::LangLit => "literal-lang",
            Kind::TypedLit => "literal-typed",
            Kind::InlineInt => "inline-int",
        }
    }
}
const KINDS: [Kind; 6] = [
    Kind::Iri,
    Kind::Blank,
    Kind::PlainLit,
    Kind::LangLit,
    Kind::TypedLit,
    Kind::InlineInt,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pos {
    S,
    P,
    O,
}
const POSITIONS: [Pos; 3] = [Pos::S, Pos::P, Pos::O];
impl Pos {
    fn name(self) -> &'static str {
        match self {
            Pos::S => "subject",
            Pos::P => "predicate",
            Pos::O => "object",
        }
    }
    fn idx(self) -> usize {
        match self {
            Pos::S => 0,
            Pos::P => 1,
            Pos::O => 2,
        }
    }
}

// --------------------------------------------------------------- parsing

/// One parsed N-Triples term: the dictionary key bytes plus its kind.
struct ParsedTerm<'a> {
    /// The bytes the dictionary keys on, in the order oxrdf would compare
    /// them: for a literal this is `lexical\x00lang-or-datatype`, so that two
    /// literals differing only in tag are distinct keys.
    key: Vec<u8>,
    kind: Kind,
    /// Only for literals: the lexical form alone, for length stats.
    lexical_len: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

/// Scan one N-Triples term starting at `i`. Returns the term and the index
/// just past it. `None` on anything unparseable (the line is then skipped).
fn scan_term(line: &[u8], mut i: usize, pos: Pos) -> Option<(ParsedTerm<'static>, usize)> {
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    if i >= line.len() {
        return None;
    }
    match line[i] {
        b'<' => {
            let start = i + 1;
            let end = memchr(line, b'>', start)?;
            let key = line[start..end].to_vec();
            let n = key.len();
            Some((
                ParsedTerm {
                    key,
                    kind: Kind::Iri,
                    lexical_len: n,
                    _marker: std::marker::PhantomData,
                },
                end + 1,
            ))
        }
        b'_' => {
            // _:label
            let start = i;
            let mut end = i;
            while end < line.len() && !matches!(line[end], b' ' | b'\t') {
                end += 1;
            }
            let key = line[start + 2..end].to_vec();
            let n = key.len();
            Some((
                ParsedTerm {
                    key,
                    kind: Kind::Blank,
                    lexical_len: n,
                    _marker: std::marker::PhantomData,
                },
                end,
            ))
        }
        b'"' => {
            if pos != Pos::O {
                return None;
            }
            // Find the closing quote, honouring backslash escapes.
            let start = i + 1;
            let mut j = start;
            loop {
                if j >= line.len() {
                    return None;
                }
                match line[j] {
                    b'\\' => j += 2,
                    b'"' => break,
                    _ => j += 1,
                }
            }
            let lexical = &line[start..j];
            let mut k = j + 1;
            let (kind, tag): (Kind, &[u8]) = if k < line.len() && line[k] == b'@' {
                let ts = k + 1;
                let mut te = ts;
                while te < line.len() && !matches!(line[te], b' ' | b'\t') {
                    te += 1;
                }
                k = te;
                (Kind::LangLit, &line[ts..te])
            } else if k + 1 < line.len() && line[k] == b'^' && line[k + 1] == b'^' {
                let ts = memchr(line, b'<', k)? + 1;
                let te = memchr(line, b'>', ts)?;
                k = te + 1;
                (Kind::TypedLit, &line[ts..te])
            } else {
                (Kind::PlainLit, b"")
            };
            let kind = if kind == Kind::TypedLit && tag == XSD_INTEGER.as_bytes() {
                if is_canonical_i32(lexical) {
                    Kind::InlineInt
                } else {
                    Kind::TypedLit
                }
            } else {
                kind
            };
            let mut key = Vec::with_capacity(lexical.len() + 1 + tag.len());
            key.extend_from_slice(lexical);
            key.push(0);
            key.extend_from_slice(tag);
            Some((
                ParsedTerm {
                    key,
                    kind,
                    lexical_len: lexical.len(),
                    _marker: std::marker::PhantomData,
                },
                k,
            ))
        }
        _ => None,
    }
}

#[inline]
fn memchr(h: &[u8], needle: u8, from: usize) -> Option<usize> {
    h[from..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

/// Mirrors `try_inline_int`: parses as i32 *and* is that i32's canonical form.
fn is_canonical_i32(lexical: &[u8]) -> bool {
    let s = match std::str::from_utf8(lexical) {
        Ok(s) => s,
        Err(_) => return false,
    };
    match s.parse::<i32>() {
        Ok(v) => v.to_string() == s,
        Err(_) => false,
    }
}

// ---------------------------------------------------- length distribution

/// Byte-length histogram; exact for lengths < 4096, bucketed above.
struct LenHist {
    small: Vec<u64>,
    large: Vec<(usize, u64)>,
    count: u64,
    sum: u128,
    max: usize,
}
impl LenHist {
    fn new() -> Self {
        LenHist {
            small: vec![0; 4096],
            large: Vec::new(),
            count: 0,
            sum: 0,
            max: 0,
        }
    }
    #[inline]
    fn add(&mut self, len: usize, weight: u64) {
        self.count += weight;
        self.sum += (len as u128) * (weight as u128);
        if len > self.max {
            self.max = len;
        }
        if len < 4096 {
            self.small[len] += weight;
        } else {
            match self.large.iter_mut().find(|(l, _)| *l == len) {
                Some((_, c)) => *c += weight,
                None => self.large.push((len, weight)),
            }
        }
    }
    fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }
    fn quantile(&self, q: f64) -> usize {
        if self.count == 0 {
            return 0;
        }
        let target = (q * self.count as f64).ceil() as u64;
        let mut seen = 0u64;
        for (len, &c) in self.small.iter().enumerate() {
            seen += c;
            if seen >= target {
                return len;
            }
        }
        let mut big = self.large.clone();
        big.sort_unstable();
        for (len, c) in big {
            seen += c;
            if seen >= target {
                return len;
            }
        }
        self.max
    }
}

// -------------------------------------------------------- cache simulation

/// Repeat-cache simulations, all driven from the same term stream.
///
/// Three designs share each cache size, so the numbers are directly
/// comparable and the cost of each design choice is isolated:
///
/// * **short-key direct-mapped** — index = hash(len, first 8 bytes), the
///   design HDB-57 R3 asserts. The slot stores the full term, so a probe that
///   matches the short key still compares the whole term; a mismatch is an
///   ordinary miss. Sound.
/// * **short-key unverified** — same index, but the slot stores only the short
///   key and the id, and a short-key match returns that id *without* comparing
///   the rest. Two distinct terms sharing length and first 8 bytes then
///   produce a **wrong** answer; `false_hits` counts those. Simulated to price
///   the "skip the compare" shortcut, not because it is usable.
/// * **full-hash direct-mapped** — index = hash(whole term), verified. The
///   control: same capacity, same replacement policy, only the index changes.
///   The gap against the short-key row is exactly what truncating the key
///   costs.
/// * **full-hash 4-way LRU** — index = hash(whole term) over `size/4` sets.
///   Shows what associativity buys on top of a better index.
struct CacheSim {
    size: usize,
    mask: usize,
    // short-key direct-mapped, verified
    slots: Vec<u32>,
    hits: u64,
    // short-key unverified
    ukeys: Vec<u64>,
    uslots: Vec<u32>,
    false_hits: u64,
    // full-hash direct-mapped, verified
    fslots: Vec<u32>,
    fhits: u64,
    // full-hash 4-way LRU (ways stored MRU-first within each set)
    ways: Vec<u32>,
    set_mask: usize,
    whits: u64,

    probes: u64,
}
const EMPTY: u32 = u32::MAX;
const WAYS: usize = 4;
impl CacheSim {
    fn new(size: usize) -> Self {
        assert!(size.is_power_of_two() && size >= WAYS);
        CacheSim {
            size,
            mask: size - 1,
            slots: vec![EMPTY; size],
            hits: 0,
            ukeys: vec![0; size],
            uslots: vec![EMPTY; size],
            false_hits: 0,
            fslots: vec![EMPTY; size],
            fhits: 0,
            ways: vec![EMPTY; size],
            set_mask: (size / WAYS) - 1,
            whits: 0,
            probes: 0,
        }
    }
    #[inline]
    fn probe(&mut self, short_key: u64, full_hash: u64, id: u32) {
        self.probes += 1;

        // -- short-key direct-mapped, verified
        let slot = (hash_u64(short_key) as usize) & self.mask;
        if self.slots[slot] == id {
            self.hits += 1;
        } else {
            self.slots[slot] = id;
        }

        // -- short-key unverified: a short-key match on a different term is a
        //    wrong answer, not a miss.
        if self.uslots[slot] != EMPTY && self.ukeys[slot] == short_key && self.uslots[slot] != id {
            self.false_hits += 1;
        }
        self.ukeys[slot] = short_key;
        self.uslots[slot] = id;

        // -- full-hash direct-mapped, verified
        let fslot = (full_hash as usize) & self.mask;
        if self.fslots[fslot] == id {
            self.fhits += 1;
        } else {
            self.fslots[fslot] = id;
        }

        // -- full-hash 4-way LRU
        let set = ((full_hash >> 32) as usize) & self.set_mask;
        let base = set * WAYS;
        let mut found = WAYS;
        for w in 0..WAYS {
            if self.ways[base + w] == id {
                found = w;
                break;
            }
        }
        if found < WAYS {
            self.whits += 1;
        }
        let promote_from = if found < WAYS { found } else { WAYS - 1 };
        for w in (1..=promote_from).rev() {
            self.ways[base + w] = self.ways[base + w - 1];
        }
        self.ways[base] = id;
    }
    fn hit_rate(&self) -> f64 {
        self.hits as f64 / self.probes.max(1) as f64
    }
    fn full_hit_rate(&self) -> f64 {
        self.fhits as f64 / self.probes.max(1) as f64
    }
    fn lru_hit_rate(&self) -> f64 {
        self.whits as f64 / self.probes.max(1) as f64
    }
}

#[inline]
fn hash_u64(mut x: u64) -> u64 {
    // splitmix64 finalizer
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    x
}

/// `(len, first 8 bytes)` packed into one u64: the low 56 bits are the first
/// 8 bytes folded, the top 8 bits are `min(len, 255)`.
#[inline]
fn short_key(key: &[u8]) -> u64 {
    let mut first = [0u8; 8];
    let n = key.len().min(8);
    first[..n].copy_from_slice(&key[..n]);
    let f = u64::from_le_bytes(first);
    ((key.len().min(255) as u64) << 56) | (f & 0x00ff_ffff_ffff_ffff)
}

// ------------------------------------------------------------- interner

/// Arena-backed string interner: one `Vec<u8>` holds every distinct term's
/// bytes end to end, and an open-addressed table maps a term to its dense id.
///
/// The obvious `HashMap<Vec<u8>, u32>` costs ~72 B of allocator and header
/// overhead per distinct term on top of the bytes themselves. On a corpus with
/// tens of millions of distinct terms that is several GB of pure overhead, so
/// the arena is what makes a full DBpedia pass fit in memory.
struct Interner {
    arena: Vec<u8>,
    /// Per-id: byte offset into `arena`, length, kind, and the lexical length
    /// (which differs from the key length for tagged literals).
    off: Vec<u64>,
    len: Vec<u32>,
    kind: Vec<Kind>,
    lex_len: Vec<u32>,
    /// Open-addressed table of ids; `EMPTY_SLOT` marks a free slot.
    table: Vec<u32>,
    mask: usize,
    /// Cached hash per slot, so a probe usually avoids touching the arena.
    slot_hash: Vec<u64>,
}
const EMPTY_SLOT: u32 = u32::MAX;

impl Interner {
    fn new() -> Self {
        let cap = 1 << 20;
        Interner {
            arena: Vec::new(),
            off: Vec::new(),
            len: Vec::new(),
            kind: Vec::new(),
            lex_len: Vec::new(),
            table: vec![EMPTY_SLOT; cap],
            mask: cap - 1,
            slot_hash: vec![0; cap],
        }
    }
    fn len(&self) -> usize {
        self.off.len()
    }
    #[inline]
    fn bytes(&self, id: u32) -> &[u8] {
        let o = self.off[id as usize] as usize;
        &self.arena[o..o + self.len[id as usize] as usize]
    }
    /// Returns `(id, is_new)`.
    #[inline]
    fn intern(&mut self, key: &[u8], h: u64, kind: Kind, lex_len: usize) -> (u32, bool) {
        let mut i = (h as usize) & self.mask;
        loop {
            let id = self.table[i];
            if id == EMPTY_SLOT {
                break;
            }
            if self.slot_hash[i] == h && self.bytes(id) == key {
                return (id, false);
            }
            i = (i + 1) & self.mask;
        }
        let id = self.off.len() as u32;
        assert!(id != EMPTY_SLOT, "more than 4 billion distinct terms");
        self.off.push(self.arena.len() as u64);
        self.len.push(key.len() as u32);
        self.kind.push(kind);
        self.lex_len.push(lex_len as u32);
        self.arena.extend_from_slice(key);
        self.table[i] = id;
        self.slot_hash[i] = h;
        if self.off.len() * 2 > self.table.len() {
            self.grow();
        }
        (id, true)
    }
    fn grow(&mut self) {
        let cap = self.table.len() * 2;
        let mut table = vec![EMPTY_SLOT; cap];
        let mut slot_hash = vec![0u64; cap];
        let mask = cap - 1;
        for (old_slot, &id) in self.table.iter().enumerate() {
            if id == EMPTY_SLOT {
                continue;
            }
            let h = self.slot_hash[old_slot];
            let mut i = (h as usize) & mask;
            while table[i] != EMPTY_SLOT {
                i = (i + 1) & mask;
            }
            table[i] = id;
            slot_hash[i] = h;
        }
        self.table = table;
        self.slot_hash = slot_hash;
        self.mask = mask;
    }
}

// ------------------------------------------------------------------- main

struct KindStats {
    occ: u64,
    distinct: u64,
    occ_len: LenHist,
    distinct_len: LenHist,
    distinct_lexical_len: LenHist,
}
impl KindStats {
    fn new() -> Self {
        KindStats {
            occ: 0,
            distinct: 0,
            occ_len: LenHist::new(),
            distinct_len: LenHist::new(),
            distinct_lexical_len: LenHist::new(),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut name = String::from("corpus");
    let mut files: Vec<String> = Vec::new();
    let mut limit_lines: u64 = u64::MAX;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                name = args[i + 1].clone();
                i += 2;
            }
            "--max-lines" => {
                limit_lines = args[i + 1].parse().unwrap();
                i += 2;
            }
            other => {
                files.push(other.to_string());
                i += 1;
            }
        }
    }
    if files.is_empty() {
        eprintln!("usage: corpus_term_stats --name NAME file.nt [...]  ('-' = stdin)");
        std::process::exit(2);
    }

    // Distinct-term table: key bytes -> dense id. Also remembers each term's
    // kind and length so the distinct-side histograms can be built at the end.
    let mut ids = Interner::new();

    let mut kind_stats: Vec<KindStats> = KINDS.iter().map(|_| KindStats::new()).collect();
    // per-position: occurrences, and a distinct-set of ids
    let mut pos_occ = [0u64; 3];
    let mut pos_distinct: [FxMap<u32, ()>; 3] =
        [FxMap::default(), FxMap::default(), FxMap::default()];

    let mut caches: Vec<CacheSim> = [256usize, 1024, 4096, 16384, 65536, 262_144]
        .iter()
        .map(|&s| CacheSim::new(s))
        .collect();

    let mut lines: u64 = 0;
    let mut skipped: u64 = 0;

    for f in &files {
        let reader: Box<dyn Read> = if f == "-" {
            Box::new(std::io::stdin())
        } else {
            Box::new(std::fs::File::open(f).unwrap_or_else(|e| panic!("open {f}: {e}")))
        };
        let mut reader = std::io::BufReader::with_capacity(1 << 22, reader);
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 22);
        let mut chunk = vec![0u8; 1 << 22];
        loop {
            let n = reader.read(&mut chunk).expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let mut start = 0usize;
            while let Some(nl) = memchr(&buf, b'\n', start) {
                let line = &buf[start..nl];
                start = nl + 1;
                if lines >= limit_lines {
                    break;
                }
                if line.is_empty() || line[0] == b'#' {
                    continue;
                }
                lines += 1;
                let mut off = 0usize;
                let mut ok = true;
                for pos in POSITIONS {
                    match scan_term(line, off, pos) {
                        Some((t, next)) => {
                            off = next;
                            // ---- record the occurrence
                            let ki = KINDS.iter().position(|&k| k == t.kind).unwrap();
                            kind_stats[ki].occ += 1;
                            kind_stats[ki].occ_len.add(t.key.len(), 1);
                            pos_occ[pos.idx()] += 1;

                            // Inline ints never reach the dictionary, so they
                            // are neither interned nor cache-probed.
                            if t.kind == Kind::InlineInt {
                                continue;
                            }
                            let fh = hash_bytes(&t.key);
                            let (id, _new) = ids.intern(&t.key, fh, t.kind, t.lexical_len);
                            pos_distinct[pos.idx()].insert(id, ());
                            let sk = short_key(&t.key);
                            for c in caches.iter_mut() {
                                c.probe(sk, fh, id);
                            }
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    skipped += 1;
                }
                if lines % 5_000_000 == 0 {
                    eprintln!("  .. {} lines, {} distinct terms", lines, ids.len());
                }
            }
            buf.drain(..start);
            if lines >= limit_lines {
                break;
            }
        }
        if lines >= limit_lines {
            break;
        }
    }

    // ---- distinct-side histograms
    for id in 0..ids.len() {
        let k = ids.kind[id];
        let ki = KINDS.iter().position(|&x| x == k).unwrap();
        kind_stats[ki].distinct += 1;
        kind_stats[ki].distinct_len.add(ids.len[id] as usize, 1);
        kind_stats[ki]
            .distinct_lexical_len
            .add(ids.lex_len[id] as usize, 1);
    }

    // ---- prefix analysis over distinct IRIs
    let mut iri_ids: Vec<u32> = (0..ids.len() as u32)
        .filter(|&i| ids.kind[i as usize] == Kind::Iri)
        .collect();
    iri_ids.sort_unstable_by(|&a, &b| ids.bytes(a).cmp(ids.bytes(b)));
    let mut lcp_hist = LenHist::new();
    let mut iri_bytes: u128 = 0;
    for w in 0..iri_ids.len() {
        iri_bytes += ids.bytes(iri_ids[w]).len() as u128;
        let l = if w == 0 {
            0
        } else {
            common_prefix(ids.bytes(iri_ids[w - 1]), ids.bytes(iri_ids[w]))
        };
        lcp_hist.add(l, 1);
    }
    // Namespace split: everything up to and including the last '/' or '#'.
    let mut ns_counts: FxMap<Vec<u8>, u64> = FxMap::default();
    let mut ns_prefix_bytes: u128 = 0;
    for &i in &iri_ids {
        let t = ids.bytes(i);
        let cut = t
            .iter()
            .rposition(|&b| b == b'/' || b == b'#')
            .map(|p| p + 1)
            .unwrap_or(0);
        ns_prefix_bytes += cut as u128;
        *ns_counts.entry(t[..cut].to_vec()).or_insert(0) += 1;
    }
    let ns_distinct = ns_counts.len() as u64;
    let ns_unique_bytes: u128 = ns_counts.keys().map(|k| k.len() as u128).sum();

    let total_occ: u64 = kind_stats.iter().map(|k| k.occ).sum();
    let inline_occ = kind_stats[KINDS.iter().position(|&k| k == Kind::InlineInt).unwrap()].occ;
    let dict_occ = total_occ - inline_occ;
    let distinct = ids.len() as u64;

    // ---- report
    let mut out = BufWriter::new(std::io::stdout());
    let mut j = String::new();
    j.push_str("{\n");
    j.push_str(&format!("  \"corpus\": \"{name}\",\n"));
    j.push_str(&format!("  \"triples\": {lines},\n"));
    j.push_str(&format!("  \"unparsed_lines\": {skipped},\n"));
    j.push_str(&format!("  \"term_occurrences\": {total_occ},\n"));
    j.push_str(&format!("  \"inline_int_occurrences\": {inline_occ},\n"));
    j.push_str(&format!("  \"dict_occurrences\": {dict_occ},\n"));
    j.push_str(&format!("  \"distinct_terms\": {distinct},\n"));
    j.push_str(&format!(
        "  \"occ_per_distinct_all\": {:.3},\n",
        total_occ as f64 / distinct.max(1) as f64
    ));
    j.push_str(&format!(
        "  \"occ_per_distinct_dict\": {:.3},\n",
        dict_occ as f64 / distinct.max(1) as f64
    ));

    j.push_str("  \"by_position\": {\n");
    for (n, p) in POSITIONS.iter().enumerate() {
        j.push_str(&format!(
            "    \"{}\": {{ \"occurrences\": {}, \"distinct\": {}, \"occ_per_distinct\": {:.3} }}{}\n",
            p.name(),
            pos_occ[p.idx()],
            pos_distinct[p.idx()].len(),
            pos_occ[p.idx()] as f64 / pos_distinct[p.idx()].len().max(1) as f64,
            if n + 1 < POSITIONS.len() { "," } else { "" }
        ));
    }
    j.push_str("  },\n");

    j.push_str("  \"by_kind\": {\n");
    for (n, k) in KINDS.iter().enumerate() {
        let s = &kind_stats[n];
        j.push_str(&format!(
            "    \"{}\": {{ \"occurrences\": {}, \"distinct\": {}, \"occ_per_distinct\": {:.3}, \
             \"key_len_occ_mean\": {:.2}, \"key_len_distinct_mean\": {:.2}, \
             \"key_len_p50\": {}, \"key_len_p90\": {}, \"key_len_p99\": {}, \"key_len_max\": {}, \
             \"lexical_len_distinct_mean\": {:.2}, \"lexical_len_p99\": {}, \"lexical_len_max\": {} }}{}\n",
            k.name(),
            s.occ,
            s.distinct,
            s.occ as f64 / s.distinct.max(1) as f64,
            s.occ_len.mean(),
            s.distinct_len.mean(),
            s.distinct_len.quantile(0.50),
            s.distinct_len.quantile(0.90),
            s.distinct_len.quantile(0.99),
            s.distinct_len.max,
            s.distinct_lexical_len.mean(),
            s.distinct_lexical_len.quantile(0.99),
            s.distinct_lexical_len.max,
            if n + 1 < KINDS.len() { "," } else { "" }
        ));
    }
    j.push_str("  },\n");

    // occurrence-weighted length over everything the dictionary actually sees
    let mut occ_all = LenHist::new();
    let mut dist_all = LenHist::new();
    for (n, k) in KINDS.iter().enumerate() {
        if *k == Kind::InlineInt {
            continue;
        }
        let s = &kind_stats[n];
        for (l, &c) in s.occ_len.small.iter().enumerate() {
            if c > 0 {
                occ_all.add(l, c);
            }
        }
        for (l, c) in &s.occ_len.large {
            occ_all.add(*l, *c);
        }
        for (l, &c) in s.distinct_len.small.iter().enumerate() {
            if c > 0 {
                dist_all.add(l, c);
            }
        }
        for (l, c) in &s.distinct_len.large {
            dist_all.add(*l, *c);
        }
    }
    j.push_str(&format!(
        "  \"key_len_all_dict\": {{ \"occ_mean\": {:.2}, \"occ_p50\": {}, \"occ_p90\": {}, \
         \"occ_p99\": {}, \"distinct_mean\": {:.2}, \"distinct_p50\": {}, \"distinct_p90\": {}, \
         \"distinct_p99\": {}, \"distinct_max\": {} }},\n",
        occ_all.mean(),
        occ_all.quantile(0.50),
        occ_all.quantile(0.90),
        occ_all.quantile(0.99),
        dist_all.mean(),
        dist_all.quantile(0.50),
        dist_all.quantile(0.90),
        dist_all.quantile(0.99),
        dist_all.max
    ));

    j.push_str(&format!(
        "  \"iri_prefix\": {{ \"distinct_iris\": {}, \"total_bytes\": {}, \"mean_lcp_sorted\": {:.2}, \
         \"lcp_p50\": {}, \"lcp_p90\": {}, \"front_coding_saving_pct\": {:.1}, \
         \"distinct_namespaces\": {}, \"namespace_bytes_pct\": {:.1}, \
         \"namespace_split_saving_pct\": {:.1} }},\n",
        iri_ids.len(),
        iri_bytes,
        lcp_hist.mean(),
        lcp_hist.quantile(0.50),
        lcp_hist.quantile(0.90),
        if iri_bytes == 0 {
            0.0
        } else {
            100.0 * (lcp_hist.sum as f64) / (iri_bytes as f64)
        },
        ns_distinct,
        if iri_bytes == 0 {
            0.0
        } else {
            100.0 * (ns_prefix_bytes as f64) / (iri_bytes as f64)
        },
        if iri_bytes == 0 {
            0.0
        } else {
            100.0 * ((ns_prefix_bytes - ns_unique_bytes) as f64) / (iri_bytes as f64)
        }
    ));

    let oracle = (dict_occ.saturating_sub(distinct)) as f64 / dict_occ.max(1) as f64;
    j.push_str(&format!("  \"oracle_hit_rate\": {oracle:.4},\n"));
    j.push_str("  \"repeat_cache\": [\n");
    for (n, c) in caches.iter().enumerate() {
        j.push_str(&format!(
            "    {{ \"entries\": {}, \"probes\": {}, \"shortkey_dm_hit_rate\": {:.4}, \
             \"shortkey_unverified_false_hit_rate\": {:.6}, \"fullhash_dm_hit_rate\": {:.4}, \
             \"fullhash_4way_lru_hit_rate\": {:.4} }}{}\n",
            c.size,
            c.probes,
            c.hit_rate(),
            c.false_hits as f64 / c.probes.max(1) as f64,
            c.full_hit_rate(),
            c.lru_hit_rate(),
            if n + 1 < caches.len() { "," } else { "" }
        ));
    }
    j.push_str("  ]\n}\n");
    out.write_all(j.as_bytes()).unwrap();
    out.flush().unwrap();

    eprintln!("== {name}: {lines} triples, {total_occ} occurrences, {distinct} distinct");
    eprintln!(
        "   occ/distinct all={:.2} dict-only={:.2}; inline-int occ={} ({:.1}%)",
        total_occ as f64 / distinct.max(1) as f64,
        dict_occ as f64 / distinct.max(1) as f64,
        inline_occ,
        100.0 * inline_occ as f64 / total_occ.max(1) as f64
    );
    eprintln!(
        "   dict key bytes: occ-mean {:.1}, distinct-mean {:.1}, distinct p99 {}, max {}",
        occ_all.mean(),
        dist_all.mean(),
        dist_all.quantile(0.99),
        dist_all.max
    );
    eprintln!(
        "   oracle (unbounded cache) hit rate {:.2}%",
        100.0 * (dict_occ.saturating_sub(distinct)) as f64 / dict_occ.max(1) as f64
    );
    eprintln!("   entries | shortkey-DM | unverified-false | fullhash-DM | fullhash-4wayLRU");
    for c in &caches {
        eprintln!(
            "   {:>7} | {:>10.2}% | {:>15.4}% | {:>10.2}% | {:>15.2}%",
            c.size,
            100.0 * c.hit_rate(),
            100.0 * c.false_hits as f64 / c.probes.max(1) as f64,
            100.0 * c.full_hit_rate(),
            100.0 * c.lru_hit_rate()
        );
    }
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}
