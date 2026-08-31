//! `dict_base_bench` — HDB-93: which structure should back the SPEC-25 §S2
//! memory-mapped dictionary base?
//!
//! Measures the four candidates HDB-57 R8 lists, over the same term stream the
//! real `Dictionary::intern` sees:
//!
//! - `hashbrown` — fully resident control (`hashbrown::HashTable`).
//! - `openaddr` — a resident open-addressed table with explicit prefetch. The
//!   batch-32 control: `hashbrown` exposes no prefetch hook, so without this
//!   arm the "batch-32 with prefetch" column has nothing to say about hash
//!   tables.
//! - `ptrhash` — PtrHash MPHF plus a mapped 16-byte record array
//!   (64-bit fingerprint + id). HDB-57 R3's recommendation.
//! - `frontcoded` — mapped front-coded sorted blocks, binary search over block
//!   heads.
//! - `fst` — the `fst` crate's `Map`, mapped. HDB-57 R4 rejects it.
//!
//! crossed with {single, batch-32}, {hit stream, miss stream}, and
//! {with, without} the 4,096-entry 4-way LRU repeat cache HDB-57 R3/R9 F4
//! settled on.
//!
//! Spike code. It is an example, not a library: `ptr_hash`, `fst`, `hashbrown`
//! and `memmap2` are dev-dependencies of `horndb-storage` and reach no shipped
//! binary.
//!
//! Usage:
//!
//! ```text
//! # 1. corpus -> (keys.bin, stream.bin): scripts/bench/corpus_term_stats.rs --emit-keys
//! # 2. or synthesize a LUBM-shaped distinct-term set at a chosen scale
//! dict_base_bench synth   --keys 100000000 --dir DIR
//! # 3. build every structure, printing build time and bits/key
//! dict_base_bench build   --dir DIR
//! # 4. the warm matrix
//! dict_base_bench query   --dir DIR --probes 10000000 --reps 3
//! # 5. one cold-page-cache cell (run once per drop_caches)
//! dict_base_bench cold    --dir DIR --arm ptrhash --probes 2000000
//! ```

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use memmap2::Mmap;

// --------------------------------------------------------------- prefetch

/// Prefetch one address into L1. x86-64 is the only host we record numbers on
/// (hornbench); the aarch64 arm exists so the laptop still compiles and lints.
#[inline(always)]
fn prefetch(p: *const u8) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_prefetch(p as *const i8, std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = p;
    }
}

// ------------------------------------------------------------------ hashing

/// xxh3-flavoured 64-bit hash. Same role as the dictionary's own key hash: it
/// is computed once per lookup and reused by the repeat cache, the fingerprint
/// and the open-addressed table.
#[inline]
fn hash64(b: &[u8]) -> u64 {
    // A short, fast, well-mixing multiply-xor hash. Not xxh3 itself — the point
    // of the matrix is the probe, and R1 puts hashing at 4-6 ns either way.
    const K0: u64 = 0x9e37_79b9_7f4a_7c15;
    const K1: u64 = 0xc2b2_ae3d_27d4_eb4f;
    let mut h = K0 ^ (b.len() as u64).wrapping_mul(K1);
    let mut c = b.chunks_exact(8);
    for w in &mut c {
        let v = u64::from_le_bytes(w.try_into().unwrap());
        h = (h ^ v).wrapping_mul(K1);
        h = h.rotate_left(29).wrapping_add(v);
    }
    let r = c.remainder();
    if !r.is_empty() {
        let mut buf = [0u8; 8];
        buf[..r.len()].copy_from_slice(r);
        h = (h ^ u64::from_le_bytes(buf)).wrapping_mul(K1);
    }
    h ^= h >> 32;
    h = h.wrapping_mul(K0);
    h ^ (h >> 29)
}

// -------------------------------------------------------------------- keys

/// Every distinct dictionary key, end to end in one arena, in id order.
struct Keys {
    arena: Vec<u8>,
    off: Vec<u64>,
    len: Vec<u32>,
}

impl Keys {
    #[inline]
    fn get(&self, i: usize) -> &[u8] {
        let o = self.off[i] as usize;
        &self.arena[o..o + self.len[i] as usize]
    }
    fn n(&self) -> usize {
        self.off.len()
    }
    fn bytes(&self) -> usize {
        self.arena.len()
    }

    fn read(path: &Path) -> Keys {
        let mut f = std::io::BufReader::with_capacity(1 << 22, File::open(path).unwrap());
        let mut arena = Vec::new();
        let mut off = Vec::new();
        let mut len = Vec::new();
        let mut hdr = [0u8; 4];
        loop {
            match f.read_exact(&mut hdr) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("read {}: {e}", path.display()),
            }
            let l = u32::from_le_bytes(hdr) as usize;
            off.push(arena.len() as u64);
            len.push(l as u32);
            let base = arena.len();
            arena.resize(base + l, 0);
            f.read_exact(&mut arena[base..]).unwrap();
        }
        Keys { arena, off, len }
    }

    fn write(&self, path: &Path) {
        let mut w = BufWriter::with_capacity(1 << 22, File::create(path).unwrap());
        for i in 0..self.n() {
            let b = self.get(i);
            w.write_all(&(b.len() as u32).to_le_bytes()).unwrap();
            w.write_all(b).unwrap();
        }
        w.flush().unwrap();
    }

    /// Sorted order of ids by key bytes. Needed by the front-coded base and
    /// by `fst`, both of which require sorted input.
    fn sorted_ids(&self) -> Vec<u32> {
        let mut v: Vec<u32> = (0..self.n() as u32).collect();
        v.sort_unstable_by(|&a, &b| self.get(a as usize).cmp(self.get(b as usize)));
        v
    }
}

// ------------------------------------------------------- LUBM-shaped synth

/// Generate `n` distinct LUBM-shaped dictionary keys.
///
/// LUBM's vocabulary is templated, so its distinct-term set can be produced
/// without generating triples — which matters, because the real UBA generator
/// cannot reach 100M distinct terms on this hardware in reasonable time or
/// disk. The shape targets what HDB-92 F3/F5 measured on the real LUBM-100
/// set: roughly two thirds IRIs and one third plain literals, IRI keys around
/// 64 B, literal keys around 45 B, and a mean shared prefix over sorted IRIs
/// near 62 B. `build` prints the achieved profile so it can be checked against
/// the real set, which is also measured here at its own scale.
///
/// Per-department entity counts are jittered by a small LCG. Without the
/// jitter every department carries an identical dense id range, the suffix
/// automaton behind `fst` shares almost everything, and the FST comes out
/// three orders of magnitude smaller than on real data — an artifact that
/// would have flattered exactly the arm HDB-57 R4 rejects.
fn synth_keys(n: usize) -> Keys {
    const CLASSES: [&str; 8] = [
        "GraduateStudent",
        "UndergraduateStudent",
        "AssistantProfessor",
        "AssociateProfessor",
        "FullProfessor",
        "Lecturer",
        "ResearchAssistant",
        "TeachingAssistant",
    ];
    const BASE: [usize; 8] = [180, 540, 12, 14, 10, 8, 24, 20];
    const ONTO: &str = "http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#";
    let mut arena: Vec<u8> = Vec::with_capacity(n * 60);
    let mut off = Vec::with_capacity(n);
    let mut len = Vec::with_capacity(n);
    let push = |arena: &mut Vec<u8>, off: &mut Vec<u64>, len: &mut Vec<u32>, s: &str| {
        off.push(arena.len() as u64);
        len.push(s.len() as u32);
        arena.extend_from_slice(s.as_bytes());
    };
    // TBox vocabulary.
    for c in CLASSES.iter() {
        push(&mut arena, &mut off, &mut len, &format!("{ONTO}{c}"));
    }
    for p in [
        "advisor",
        "teacherOf",
        "takesCourse",
        "memberOf",
        "subOrganizationOf",
        "publicationAuthor",
        "undergraduateDegreeFrom",
        "mastersDegreeFrom",
        "doctoralDegreeFrom",
        "worksFor",
        "name",
        "emailAddress",
        "telephone",
        "researchInterest",
        "headOf",
    ] {
        push(&mut arena, &mut off, &mut len, &format!("{ONTO}{p}"));
    }
    // Names and research interests repeat across departments, so the distinct
    // set holds one copy each. A plain literal's key is `lexical` followed by a
    // NUL and an empty tag, matching what `corpus_term_stats.rs` emits.
    for c in CLASSES.iter() {
        for i in 0..600usize {
            push(&mut arena, &mut off, &mut len, &format!("{c}{i}\u{0}"));
        }
    }
    for i in 0..40usize {
        push(&mut arena, &mut off, &mut len, &format!("Research{i}\u{0}"));
    }
    let mut rng = Rng(0x1b3f_0000_0000_0001u64);
    let mut u = 0usize;
    'outer: loop {
        for d in 0..15usize {
            let dept = format!("http://www.Department{d}.University{u}.edu");
            push(&mut arena, &mut off, &mut len, &dept);
            for (ci, c) in CLASSES.iter().enumerate() {
                // +-25% jitter, so no two departments share an id range.
                let j = (rng.next() % 51) as i64 - 25;
                let per = ((BASE[ci] as i64) * (100 + j) / 100).max(1) as usize;
                for i in 0..per {
                    push(&mut arena, &mut off, &mut len, &format!("{dept}/{c}{i}"));
                    if off.len() >= n {
                        break 'outer;
                    }
                    // Unique per person: the email literal.
                    push(
                        &mut arena,
                        &mut off,
                        &mut len,
                        &format!("{c}{i}@Department{d}.University{u}.edu\u{0}"),
                    );
                    if off.len() >= n {
                        break 'outer;
                    }
                    if i % 2 == 0 {
                        push(
                            &mut arena,
                            &mut off,
                            &mut len,
                            &format!("{dept}/{c}{i}/Publication{}", i % 7),
                        );
                        if off.len() >= n {
                            break 'outer;
                        }
                    }
                }
            }
            let ncourse = 20 + (rng.next() % 21) as usize;
            for i in 0..ncourse {
                push(&mut arena, &mut off, &mut len, &format!("{dept}/Course{i}"));
                if off.len() >= n {
                    break 'outer;
                }
                push(
                    &mut arena,
                    &mut off,
                    &mut len,
                    &format!("{dept}/GraduateCourse{i}"),
                );
                if off.len() >= n {
                    break 'outer;
                }
            }
        }
        u += 1;
    }
    off.truncate(n);
    len.truncate(n);
    let end = off[n - 1] as usize + len[n - 1] as usize;
    arena.truncate(end);
    Keys { arena, off, len }
}

/// Re-instantiate a real LUBM key set at `factor` times as many universities,
/// and replay its term stream over the result.
///
/// Every LUBM key that names a university carries it as `University{k}`.
/// Rewriting `k` to `k + 100*r` for `r` in `0..factor` produces the
/// distinct-term set the real generator emits for `100*factor` universities,
/// with the real per-department irregularity, the real literal mix and the
/// real length tail. Keys naming no university (the ontology, shared names,
/// research interests) are emitted once, in a shared block up front.
///
/// Layout: `[shared][copy 0][copy 1]...`, so a source id maps to a scaled id
/// by adding its copy's base. The replayed stream switches copy every
/// `COPY_CHUNK` occurrences, which is what a real LUBM document stream looks
/// like: one university's file at a time, over a dictionary holding them all.
///
/// This is how the 10M and 100M scale points are built. Nothing is invented —
/// the shape comes from the measured LUBM-100 set.
const COPY_CHUNK: usize = 1_000_000;

fn scale_keys(src: &Keys, src_stream: &[u32], factor: usize, cap: usize) -> (Keys, Vec<u32>) {
    let needle = b"University";
    // Classify each source key and record its position within its block.
    let mut univ_at: Vec<Option<(u32, u32)>> = Vec::with_capacity(src.n());
    let mut pos: Vec<u32> = vec![0; src.n()];
    let mut n_shared = 0u32;
    let mut n_univ = 0u32;
    for i in 0..src.n() {
        let k = src.get(i);
        let mut at = None;
        let mut p = 0usize;
        while p + needle.len() <= k.len() {
            if &k[p..p + needle.len()] == needle {
                let ds = p + needle.len();
                let mut de = ds;
                while de < k.len() && k[de].is_ascii_digit() {
                    de += 1;
                }
                if de > ds {
                    at = Some((ds as u32, de as u32));
                    break;
                }
            }
            p += 1;
        }
        match at {
            None => {
                pos[i] = n_shared;
                n_shared += 1;
            }
            Some(_) => {
                pos[i] = n_univ;
                n_univ += 1;
            }
        }
        univ_at.push(at);
    }
    let ns = n_shared as usize;
    let nu = n_univ as usize;
    let want = cap.min(ns + nu * factor);
    let scaled_id = |src_id: u32, r: usize| -> usize {
        if univ_at[src_id as usize].is_none() {
            pos[src_id as usize] as usize
        } else {
            ns + r * nu + pos[src_id as usize] as usize
        }
    };

    let mut arena: Vec<u8> = Vec::with_capacity(want * 60);
    let mut off = vec![0u64; want];
    let mut len = vec![0u32; want];
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut emit = |arena: &mut Vec<u8>, id: usize, bytes: &[u8]| {
        off[id] = arena.len() as u64;
        len[id] = bytes.len() as u32;
        arena.extend_from_slice(bytes);
    };
    for i in 0..src.n() {
        if univ_at[i].is_none() {
            let id = pos[i] as usize;
            if id < want {
                emit(&mut arena, id, src.get(i));
            }
        }
    }
    'copies: for r in 0..factor {
        for i in 0..src.n() {
            let Some((ds, de)) = univ_at[i] else { continue };
            let id = scaled_id(i as u32, r);
            if id >= want {
                continue 'copies;
            }
            let k = src.get(i);
            let (ds, de) = (ds as usize, de as usize);
            let num: usize = std::str::from_utf8(&k[ds..de]).unwrap().parse().unwrap();
            buf.clear();
            buf.extend_from_slice(&k[..ds]);
            buf.extend_from_slice((num + 100 * r).to_string().as_bytes());
            buf.extend_from_slice(&k[de..]);
            emit(&mut arena, id, &buf);
        }
    }

    let mut stream = Vec::with_capacity(src_stream.len());
    let mut r = 0usize;
    for (j, &sid) in src_stream.iter().enumerate() {
        if j % COPY_CHUNK == 0 && j > 0 {
            r = (r + 1) % factor;
        }
        let id = scaled_id(sid, r);
        if id < want {
            stream.push(id as u32);
        }
    }
    (Keys { arena, off, len }, stream)
}

// ----------------------------------------------------------- probe streams

/// Build a hit stream over `n` keys with a Zipf-like popularity skew, then
/// report the 4K/4-way repeat-cache hit rate it produces.
///
/// The skew is not decorative. HDB-92 F4 measured that cache at 52.8-84.3%
/// across four corpora; a uniform stream would put it near zero and make the
/// "with repeat cache" arm meaningless. `s` is tuned by the caller to land on
/// the corpus figure the arm is supposed to represent.
fn zipf_stream(n: usize, probes: usize, s: f64, seed: u64) -> Vec<u32> {
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(probes);
    let ln_n = (n as f64).ln();
    for _ in 0..probes {
        // Inverse-transform sample of a continuous approximation to Zipf.
        let u = rng.next_f64();
        let idx = if (s - 1.0).abs() < 1e-9 {
            (ln_n * u).exp() - 1.0
        } else {
            let a = 1.0 - s;
            ((1.0 + u * ((n as f64).powf(a) - 1.0)).powf(1.0 / a)) - 1.0
        };
        let i = (idx as usize).min(n - 1);
        out.push(i as u32);
    }
    out
}

/// Uniform random hit stream: no locality at all, so every probe is a real
/// random access into the base. The pessimistic end of the range.
fn uniform_stream(n: usize, probes: usize, seed: u64) -> Vec<u32> {
    let mut rng = Rng(seed);
    (0..probes)
        .map(|_| (rng.next() % n as u64) as u32)
        .collect()
}

/// Miss keys: an existing key with a `0x01` byte appended. That byte cannot
/// occur in an IRI, a blank-node label or the tag half of a literal key, so
/// non-membership is guaranteed; and the miss stays adjacent to a real key,
/// which is the worst case for the sorted structures.
fn miss_keys(keys: &Keys, count: usize, seed: u64) -> Keys {
    let mut rng = Rng(seed);
    let mut arena = Vec::with_capacity(count * 64);
    let mut off = Vec::with_capacity(count);
    let mut len = Vec::with_capacity(count);
    for _ in 0..count {
        let i = (rng.next() % keys.n() as u64) as usize;
        let b = keys.get(i);
        off.push(arena.len() as u64);
        len.push(b.len() as u32 + 1);
        arena.extend_from_slice(b);
        arena.push(0x01);
    }
    Keys { arena, off, len }
}

struct Rng(u64);
impl Rng {
    #[inline]
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    #[inline]
    fn next_f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ------------------------------------------------------------ repeat cache

/// 4,096 entries, 4-way LRU, indexed and tagged by the full 64-bit term hash.
/// HDB-92 F4's chosen configuration. The tag is the strong fingerprint that
/// note requires, so a hit needs no string compare.
const CACHE_SETS: usize = 1024;
struct RepeatCache {
    tag: Vec<u64>,
    val: Vec<u32>,
    /// Per set: the 4 ways in most-recent-first order, packed 2 bits each.
    order: Vec<u8>,
    hits: u64,
    probes: u64,
}

impl RepeatCache {
    fn new() -> Self {
        RepeatCache {
            tag: vec![0; CACHE_SETS * 4],
            val: vec![0; CACHE_SETS * 4],
            order: vec![0b11_10_01_00; CACHE_SETS],
            hits: 0,
            probes: 0,
        }
    }
    fn reset(&mut self) {
        self.tag.iter_mut().for_each(|t| *t = 0);
        self.order.iter_mut().for_each(|o| *o = 0b11_10_01_00);
        self.hits = 0;
        self.probes = 0;
    }
    #[inline]
    fn get(&mut self, h: u64) -> Option<u32> {
        self.probes += 1;
        let set = (h as usize) & (CACHE_SETS - 1);
        let base = set * 4;
        let ord = self.order[set];
        for k in 0..4 {
            let way = ((ord >> (2 * k)) & 3) as usize;
            if self.tag[base + way] == h {
                // Promote to most-recent: the hit way moves to position 0 and
                // every way ahead of it shifts down one.
                let mut new = way as u8;
                for kk in 0..4 {
                    let w = (ord >> (2 * kk)) & 3;
                    if kk != k {
                        new |= (w as u8) << (2 * (if kk < k { kk + 1 } else { kk }));
                    }
                }
                self.order[set] = new;
                self.hits += 1;
                return Some(self.val[base + way]);
            }
        }
        None
    }
    #[inline]
    fn put(&mut self, h: u64, v: u32) {
        let set = (h as usize) & (CACHE_SETS - 1);
        let base = set * 4;
        let ord = self.order[set];
        let victim = ((ord >> 6) & 3) as usize;
        self.tag[base + victim] = h;
        self.val[base + victim] = v;
        // Victim becomes most-recent; the rest shift down one.
        let mut new = victim as u8;
        for kk in 0..3 {
            let w = (ord >> (2 * kk)) & 3;
            new |= (w as u8) << (2 * (kk + 1));
        }
        self.order[set] = new;
    }
    fn hit_rate(&self) -> f64 {
        self.hits as f64 / self.probes.max(1) as f64
    }
}

// ------------------------------------------------------- resident controls

type HashTable = hashbrown::HashTable<u32>;

fn build_hashbrown(keys: &Keys) -> HashTable {
    let mut t = HashTable::with_capacity(keys.n());
    for i in 0..keys.n() {
        let h = hash64(keys.get(i));
        t.insert_unique(h, i as u32, |&v| hash64(keys.get(v as usize)));
    }
    t
}

#[inline]
fn hashbrown_get(t: &HashTable, keys: &Keys, key: &[u8], h: u64) -> Option<u32> {
    t.find(h, |&v| keys.get(v as usize) == key).copied()
}

/// Resident open-addressed table with a cached tag per slot, so a probe reads
/// one cache line and only touches the arena on a tag match. This is the
/// prefetchable hash-table control.
struct OpenAddr {
    /// Packed slot: high 32 bits are a hash tag, low 32 the id. `!0` = empty.
    slot: Vec<u64>,
    mask: usize,
}

const EMPTY: u64 = u64::MAX;

impl OpenAddr {
    fn build(keys: &Keys) -> OpenAddr {
        let cap = (keys.n() * 8 / 7 + 16).next_power_of_two();
        let mut t = OpenAddr {
            slot: vec![EMPTY; cap],
            mask: cap - 1,
        };
        for i in 0..keys.n() {
            let h = hash64(keys.get(i));
            let mut p = (h as usize) & t.mask;
            while t.slot[p] != EMPTY {
                p = (p + 1) & t.mask;
            }
            t.slot[p] = ((h >> 32) << 32) | i as u64;
        }
        t
    }
    #[inline]
    fn start(&self, h: u64) -> usize {
        (h as usize) & self.mask
    }
    #[inline]
    fn get_at(&self, mut p: usize, keys: &Keys, key: &[u8], h: u64) -> Option<u32> {
        let tag = h >> 32;
        loop {
            let s = self.slot[p];
            if s == EMPTY {
                return None;
            }
            if (s >> 32) == tag {
                let id = s as u32;
                if keys.get(id as usize) == key {
                    return Some(id);
                }
            }
            p = (p + 1) & self.mask;
        }
    }
    fn bytes(&self) -> usize {
        self.slot.len() * 8
    }
}

// ------------------------------------------------------------ PtrHash base

type Mph<'a> = ptr_hash::PtrHash<
    &'a [u8],
    ptr_hash::bucket_fn::CubicEps,
    Vec<u32>,
    ptr_hash::hash::Xxh3_128,
    Vec<u8>,
    true,
    true,
>;

/// One mapped slot: 64-bit fingerprint, id, and the id's arena offset index.
/// 16 bytes so a slot never straddles a cache line — one random probe answers
/// membership and the id together, which is the whole point of R3's layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rec {
    fp: u64,
    id: u32,
    _pad: u32,
}

const REC_SIZE: usize = std::mem::size_of::<Rec>();

// -------------------------------------------------------- front-coded base

/// Keys per front-coded block. 16 is HDT's shape: small enough that the
/// in-block scan is a handful of cache lines, large enough that the block-head
/// array the binary search walks stays 16x smaller than the key set.
const BLOCK: usize = 16;

/// File layout, all little-endian:
///
/// ```text
///   u64 n_keys | u64 n_blocks
///   u64 head_off[n_blocks + 1]     -- into the heads region
///   u64 body_off[n_blocks + 1]     -- into the bodies region
///   heads:  the full first key of every block, concatenated
///   bodies: per block, u32 id[k], then for keys 1..k
///           varint(lcp) varint(suffix_len) suffix
/// ```
struct FrontCoded<'a> {
    n_keys: usize,
    n_blocks: usize,
    head_off: &'a [u64],
    body_off: &'a [u64],
    heads: &'a [u8],
    bodies: &'a [u8],
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

#[inline]
fn get_varint(b: &[u8], i: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut s = 0u32;
    loop {
        let c = b[*i];
        *i += 1;
        v |= ((c & 0x7f) as u64) << s;
        if c & 0x80 == 0 {
            return v;
        }
        s += 7;
    }
}

fn build_front_coded(keys: &Keys, sorted: &[u32]) -> Vec<u8> {
    let n = sorted.len();
    let n_blocks = n.div_ceil(BLOCK);
    let mut heads: Vec<u8> = Vec::with_capacity(n_blocks * 64);
    let mut head_off: Vec<u64> = Vec::with_capacity(n_blocks + 1);
    let mut bodies: Vec<u8> = Vec::with_capacity(n * 16);
    let mut body_off: Vec<u64> = Vec::with_capacity(n_blocks + 1);
    for b in 0..n_blocks {
        let lo = b * BLOCK;
        let hi = (lo + BLOCK).min(n);
        head_off.push(heads.len() as u64);
        heads.extend_from_slice(keys.get(sorted[lo] as usize));
        body_off.push(bodies.len() as u64);
        bodies.push((hi - lo) as u8);
        for r in lo..hi {
            bodies.extend_from_slice(&sorted[r].to_le_bytes());
        }
        for r in (lo + 1)..hi {
            let prev = keys.get(sorted[r - 1] as usize);
            let cur = keys.get(sorted[r] as usize);
            let lcp = prev
                .iter()
                .zip(cur.iter())
                .take_while(|(a, b)| a == b)
                .count();
            put_varint(&mut bodies, lcp as u64);
            put_varint(&mut bodies, (cur.len() - lcp) as u64);
            bodies.extend_from_slice(&cur[lcp..]);
        }
    }
    head_off.push(heads.len() as u64);
    body_off.push(bodies.len() as u64);

    let mut out = Vec::with_capacity(64 + heads.len() + bodies.len() + 16 * n_blocks);
    out.extend_from_slice(&(n as u64).to_le_bytes());
    out.extend_from_slice(&(n_blocks as u64).to_le_bytes());
    for v in &head_off {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &body_off {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&heads);
    out.extend_from_slice(&bodies);
    out
}

fn read_u64s(b: &[u8], off: usize, n: usize) -> &[u64] {
    let (pre, mid, _) = unsafe { b[off..off + n * 8].align_to::<u64>() };
    assert!(pre.is_empty(), "misaligned offset table");
    mid
}

impl<'a> FrontCoded<'a> {
    fn open(b: &'a [u8]) -> FrontCoded<'a> {
        let n_keys = u64::from_le_bytes(b[0..8].try_into().unwrap()) as usize;
        let n_blocks = u64::from_le_bytes(b[8..16].try_into().unwrap()) as usize;
        let ho = 16;
        let bo = ho + (n_blocks + 1) * 8;
        let heads_at = bo + (n_blocks + 1) * 8;
        let head_off = read_u64s(b, ho, n_blocks + 1);
        let body_off = read_u64s(b, bo, n_blocks + 1);
        let heads_len = head_off[n_blocks] as usize;
        FrontCoded {
            n_keys,
            n_blocks,
            head_off,
            body_off,
            heads: &b[heads_at..heads_at + heads_len],
            bodies: &b[heads_at + heads_len..],
        }
    }
    #[inline]
    fn head(&self, b: usize) -> &[u8] {
        &self.heads[self.head_off[b] as usize..self.head_off[b + 1] as usize]
    }
    /// Largest block whose head is <= key, or `None` if key sorts before all.
    #[inline]
    fn locate(&self, key: &[u8]) -> Option<usize> {
        let mut lo = 0usize;
        let mut hi = self.n_blocks;
        if self.head(0) > key {
            return None;
        }
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.head(mid) <= key {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(lo)
    }
    /// Scan one block for `key`.
    #[inline]
    fn scan(&self, b: usize, key: &[u8], buf: &mut Vec<u8>) -> Option<u32> {
        let mut i = self.body_off[b] as usize;
        let k = self.bodies[i] as usize;
        i += 1;
        let ids_at = i;
        i += k * 4;
        let head = self.head(b);
        if head == key {
            return Some(u32::from_le_bytes(
                self.bodies[ids_at..ids_at + 4].try_into().unwrap(),
            ));
        }
        buf.clear();
        buf.extend_from_slice(head);
        for r in 1..k {
            let lcp = get_varint(self.bodies, &mut i) as usize;
            let sl = get_varint(self.bodies, &mut i) as usize;
            buf.truncate(lcp);
            buf.extend_from_slice(&self.bodies[i..i + sl]);
            i += sl;
            match buf.as_slice().cmp(key) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    let o = ids_at + r * 4;
                    return Some(u32::from_le_bytes(
                        self.bodies[o..o + 4].try_into().unwrap(),
                    ));
                }
                std::cmp::Ordering::Greater => return None,
            }
        }
        None
    }
    #[inline]
    fn get(&self, key: &[u8], buf: &mut Vec<u8>) -> Option<u32> {
        let b = self.locate(key)?;
        self.scan(b, key, buf)
    }
    /// Level-synchronous batch binary search: at each level, prefetch every
    /// query's next block head before reading any of them. This is R5's
    /// memory-level-parallelism lever applied to a search structure.
    fn get_batch(&self, batch: &[&[u8]], out: &mut Vec<Option<u32>>, buf: &mut Vec<u8>) {
        let b = batch.len();
        let mut lo = vec![0usize; b];
        let mut hi = vec![self.n_blocks; b];
        let levels = (usize::BITS - self.n_blocks.leading_zeros()) as usize + 1;
        for _ in 0..levels {
            for j in 0..b {
                if hi[j] - lo[j] > 1 {
                    let mid = (lo[j] + hi[j]) / 2;
                    prefetch(unsafe { self.head_off.as_ptr().add(mid) as *const u8 });
                }
            }
            for j in 0..b {
                if hi[j] - lo[j] > 1 {
                    let mid = (lo[j] + hi[j]) / 2;
                    prefetch(unsafe { self.heads.as_ptr().add(self.head_off[mid] as usize) });
                }
            }
            for j in 0..b {
                if hi[j] - lo[j] > 1 {
                    let mid = (lo[j] + hi[j]) / 2;
                    if self.head(mid) <= batch[j] {
                        lo[j] = mid;
                    } else {
                        hi[j] = mid;
                    }
                }
            }
        }
        for j in 0..b {
            prefetch(unsafe { self.bodies.as_ptr().add(self.body_off[lo[j]] as usize) });
        }
        out.clear();
        for j in 0..b {
            if self.head(0) > batch[j] {
                out.push(None);
            } else {
                out.push(self.scan(lo[j], batch[j], buf));
            }
        }
    }
}

// ------------------------------------------------------------------ timing

struct Cell {
    ns: Vec<f64>,
    cache_hit: f64,
    checksum: u64,
}

impl Cell {
    fn median(&self) -> f64 {
        let mut v = self.ns.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }
    fn lo(&self) -> f64 {
        self.ns.iter().cloned().fold(f64::INFINITY, f64::min)
    }
    fn hi(&self) -> f64 {
        self.ns.iter().cloned().fold(0.0, f64::max)
    }
}

fn peak_rss_mib() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for l in s.lines() {
                if let Some(r) = l.strip_prefix("VmHWM:") {
                    let kb: f64 = r
                        .trim()
                        .trim_end_matches(" kB")
                        .trim()
                        .parse()
                        .unwrap_or(0.0);
                    return kb / 1024.0;
                }
            }
        }
    }
    0.0
}

// -------------------------------------------------------------------- main

fn map(path: &Path) -> Mmap {
    let f = File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    unsafe { Mmap::map(&f).unwrap() }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: dict_base_bench <synth|build|query|cold> --dir DIR [...]");
        std::process::exit(2);
    }
    let mode = args[1].clone();
    let mut dir = PathBuf::from("/tmp/hdb93");
    let mut n_keys = 10_000_000usize;
    let mut probes = 10_000_000usize;
    let mut reps = 3usize;
    let mut arm = String::from("all");
    let mut zipf = 0.92f64;
    let mut src_dir = String::new();
    let mut factor = 1usize;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--keys" => {
                n_keys = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--probes" => {
                probes = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--reps" => {
                reps = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--arm" => {
                arm = args[i + 1].clone();
                i += 2;
            }
            "--src" => {
                src_dir = args[i + 1].clone();
                i += 2;
            }
            "--factor" => {
                factor = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--zipf" => {
                zipf = args[i + 1].parse().unwrap();
                i += 2;
            }
            o => panic!("unknown arg {o}"),
        }
    }
    std::fs::create_dir_all(&dir).unwrap();
    match mode.as_str() {
        "synth" => do_synth(&dir, n_keys),
        "scale" => {
            let sd = PathBuf::from(&src_dir);
            let src = Keys::read(&sd.join("keys.bin"));
            let raw = std::fs::read(sd.join("stream.bin")).unwrap();
            let src_stream: Vec<u32> = raw
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let (k, stream) = scale_keys(&src, &src_stream, factor, n_keys);
            eprintln!(
                "scale: {} keys x{factor} -> {} keys, {:.1} B/key, {} stream entries",
                src.n(),
                k.n(),
                k.bytes() as f64 / k.n() as f64,
                stream.len()
            );
            k.write(&dir.join("keys.bin"));
            let mut w =
                BufWriter::with_capacity(1 << 22, File::create(dir.join("stream.bin")).unwrap());
            for id in &stream {
                w.write_all(&id.to_le_bytes()).unwrap();
            }
            w.flush().unwrap();
        }
        "build" => do_build(&dir),
        "query" => do_query(&dir, probes, reps, zipf, &arm),
        "cold" => do_cold(&dir, probes, &arm, zipf),
        o => panic!("unknown mode {o}"),
    }
}

fn do_synth(dir: &Path, n: usize) {
    let t = Instant::now();
    let k = synth_keys(n);
    eprintln!(
        "synth: {} keys, {} arena bytes, {:.1} B/key, {:.2}s",
        k.n(),
        k.bytes(),
        k.bytes() as f64 / k.n() as f64,
        t.elapsed().as_secs_f64()
    );
    k.write(&dir.join("keys.bin"));
}

fn key_stats(keys: &Keys, sorted: &[u32]) {
    let mut lens: Vec<u32> = keys.len.clone();
    lens.sort_unstable();
    let q = |p: f64| lens[((lens.len() as f64 - 1.0) * p) as usize];
    let mean = keys.bytes() as f64 / keys.n() as f64;
    let mut lcp_total: u64 = 0;
    for w in 1..sorted.len() {
        let a = keys.get(sorted[w - 1] as usize);
        let b = keys.get(sorted[w] as usize);
        lcp_total += a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count() as u64;
    }
    println!(
        "keys: n={} bytes={} mean={:.1} p50={} p90={} p99={} max={} mean-LCP={:.1} \
         front-coding-saving={:.1}%",
        keys.n(),
        keys.bytes(),
        mean,
        q(0.50),
        q(0.90),
        q(0.99),
        lens[lens.len() - 1],
        lcp_total as f64 / (sorted.len() - 1) as f64,
        100.0 * lcp_total as f64 / keys.bytes() as f64,
    );
}

fn do_build(dir: &Path) {
    let t0 = Instant::now();
    let keys = Keys::read(&dir.join("keys.bin"));
    eprintln!(
        "loaded {} keys in {:.2}s",
        keys.n(),
        t0.elapsed().as_secs_f64()
    );
    let n = keys.n();

    let t = Instant::now();
    let sorted = keys.sorted_ids();
    let t_sort = t.elapsed().as_secs_f64();
    key_stats(&keys, &sorted);
    println!("build sort-keys                {t_sort:8.2}s");

    // --- ptrhash, balanced (CubicEps, single part, minimal)
    let refs: Vec<&[u8]> = (0..n).map(|i| keys.get(i)).collect();
    let t = Instant::now();
    let mph = <Mph>::new(&refs, ptr_hash::PtrHashParams::default_balanced());
    let t_mph = t.elapsed().as_secs_f64();
    let (b_pilots, b_remap) = mph.bits_per_element();
    println!(
        "build ptrhash-balanced         {t_mph:8.2}s   {:.3} bits/key \
         (pilots {b_pilots:.3}, remap {b_remap:.3})",
        b_pilots + b_remap
    );

    // --- ptrhash, compact: multi-part, multi-threaded construction. The
    // checkpoint-cadence question is whether the parallel build is seconds.
    let t = Instant::now();
    let cmph = <ptr_hash::CompactPtrHash<ptr_hash::hash::Xxh3_128, &[u8]>>::new(
        &refs,
        ptr_hash::PtrHashParams::default_compact(),
    );
    let t_cmph = t.elapsed().as_secs_f64();
    let (c_p, c_r) = cmph.bits_per_element();
    println!(
        "build ptrhash-compact(mt)      {t_cmph:8.2}s   {:.3} bits/key \
         (pilots {c_p:.3}, remap {c_r:.3})",
        c_p + c_r
    );
    drop(cmph);

    // --- the mapped record array for the balanced MPHF
    let t = Instant::now();
    let mut recs = vec![
        Rec {
            fp: 0,
            id: u32::MAX,
            _pad: 0
        };
        n
    ];
    for i in 0..n {
        let k = keys.get(i);
        let slot = mph.index(&k);
        recs[slot] = Rec {
            fp: hash64(k),
            id: i as u32,
            _pad: 0,
        };
    }
    let rec_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(recs.as_ptr() as *const u8, std::mem::size_of_val(&recs[..]))
    };
    std::fs::write(dir.join("ptrhash_recs.bin"), rec_bytes).unwrap();
    // The MPHF itself is small and stays resident; persist the key order it
    // was built from so `query` can rebuild it deterministically.
    let t_recs = t.elapsed().as_secs_f64();
    println!("build ptrhash-records          {t_recs:8.2}s   {REC_SIZE} B/key mapped");
    drop(recs);
    drop(refs);

    // --- front-coded
    let t = Instant::now();
    let fc = build_front_coded(&keys, &sorted);
    let t_fc = t.elapsed().as_secs_f64();
    std::fs::write(dir.join("frontcoded.bin"), &fc).unwrap();
    println!(
        "build frontcoded               {t_fc:8.2}s   {:.2} B/key total file",
        fc.len() as f64 / n as f64
    );
    drop(fc);

    // --- fst
    let t = Instant::now();
    let f = File::create(dir.join("map.fst")).unwrap();
    let mut b = fst::MapBuilder::new(BufWriter::with_capacity(1 << 22, f)).unwrap();
    for &r in &sorted {
        b.insert(keys.get(r as usize), r as u64).unwrap();
    }
    let mut w = b.into_inner().unwrap();
    w.flush().unwrap();
    drop(w);
    let t_fst = t.elapsed().as_secs_f64();
    {
        let m = map(&dir.join("map.fst"));
        let fm = fst::Map::new(&m[..]).unwrap();
        assert_eq!(fm.len(), n, "fst did not take every key");
    }
    println!(
        "build fst                      {t_fst:8.2}s   {:.2} B/key total file",
        file_len(&dir.join("map.fst")) as f64 / n as f64
    );

    // --- flat mapped arena, for id -> term and for the ptrhash verify path
    let t = Instant::now();
    let mut off: Vec<u64> = Vec::with_capacity(n + 1);
    let mut acc = 0u64;
    for i in 0..n {
        off.push(acc);
        acc += keys.len[i] as u64;
    }
    off.push(acc);
    let off_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(off.as_ptr() as *const u8, std::mem::size_of_val(&off[..]))
    };
    std::fs::write(dir.join("arena_off.bin"), off_bytes).unwrap();
    std::fs::write(dir.join("arena.bin"), &keys.arena).unwrap();
    println!(
        "build flat-arena               {:8.2}s   {:.2} B/key ({:.2} keys + {:.2} offsets)",
        t.elapsed().as_secs_f64(),
        (keys.arena.len() + off.len() * 8) as f64 / n as f64,
        keys.arena.len() as f64 / n as f64,
        off.len() as f64 * 8.0 / n as f64
    );

    // --- resident controls, for the build-time column only
    let t = Instant::now();
    let hb = build_hashbrown(&keys);
    println!(
        "build hashbrown (resident)     {:8.2}s   {:.2} B/key table + {:.2} B/key arena",
        t.elapsed().as_secs_f64(),
        (hb.capacity() * 5) as f64 / n as f64,
        keys.arena.len() as f64 / n as f64
    );
    drop(hb);
    let t = Instant::now();
    let oa = OpenAddr::build(&keys);
    println!(
        "build openaddr (resident)      {:8.2}s   {:.2} B/key table + {:.2} B/key arena",
        t.elapsed().as_secs_f64(),
        oa.bytes() as f64 / n as f64,
        keys.arena.len() as f64 / n as f64
    );
    drop(oa);

    println!(
        "build TOTAL wall               {:8.2}s",
        t0.elapsed().as_secs_f64()
    );
    println!("build peak RSS                 {:8.0} MiB", peak_rss_mib());
}

/// Rebuild the resident MPHF from the key file. This is what a reopen pays if
/// the MPHF is not persisted; `build` reports the same number.
fn load_mph<'a>(refs: &'a [&'a [u8]]) -> Mph<'a> {
    <Mph>::new(refs, ptr_hash::PtrHashParams::default_balanced())
}

#[allow(clippy::too_many_arguments)]
fn run_matrix(
    keys: &Keys,
    dir: &Path,
    stream: &[u32],
    misses: &Keys,
    reps: usize,
    arm: &str,
    label: &str,
) {
    let n = keys.n();
    let probes = stream.len();
    let want = |a: &str| arm == "all" || arm == a;

    // Probe key slices for the hit stream and the miss stream.
    let hit_keys: Vec<&[u8]> = stream.iter().map(|&i| keys.get(i as usize)).collect();
    let miss_keys: Vec<&[u8]> = (0..probes).map(|i| misses.get(i % misses.n())).collect();
    let hit_hash: Vec<u64> = hit_keys.iter().map(|k| hash64(k)).collect();
    let miss_hash: Vec<u64> = miss_keys.iter().map(|k| hash64(k)).collect();

    let mut cache = RepeatCache::new();
    let mut rows: Vec<(String, Cell)> = Vec::new();

    macro_rules! cell {
        ($name:expr, $body:expr) => {{
            let mut ns = Vec::new();
            let mut sum = 0u64;
            let mut chr = 0.0;
            for _ in 0..reps {
                cache.reset();
                let t = Instant::now();
                let s = $body(&mut cache);
                let e = t.elapsed().as_secs_f64();
                sum = sum.wrapping_add(s);
                ns.push(e * 1e9 / probes as f64);
                chr = cache.hit_rate();
            }
            rows.push((
                $name.to_string(),
                Cell {
                    ns,
                    cache_hit: chr,
                    checksum: sum,
                },
            ));
        }};
    }

    // ---------------- (a) hashbrown, resident
    if want("hashbrown") {
        let hb = build_hashbrown(keys);
        cell!("hashbrown/single/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += hashbrown_get(&hb, keys, hit_keys[j], hit_hash[j]).unwrap_or(0) as u64;
            }
            s
        });
        cell!("hashbrown/single/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += hashbrown_get(&hb, keys, miss_keys[j], miss_hash[j]).unwrap_or(1) as u64;
            }
            s
        });
        cell!("hashbrown/batch32/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            while j + 32 <= probes {
                for k in 0..32 {
                    s += hashbrown_get(&hb, keys, hit_keys[j + k], hit_hash[j + k]).unwrap_or(0)
                        as u64;
                }
                j += 32;
            }
            s
        });
        cell!("hashbrown/single/hit/cache", |c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let h = hit_hash[j];
                let v = match c.get(h) {
                    Some(v) => v,
                    None => {
                        let v = hashbrown_get(&hb, keys, hit_keys[j], h).unwrap_or(0);
                        c.put(h, v);
                        v
                    }
                };
                s += v as u64;
            }
            s
        });
    }

    // ---------------- (a2) open-addressed, resident, prefetchable
    if want("openaddr") {
        let oa = OpenAddr::build(keys);
        cell!("openaddr/single/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let p = oa.start(hit_hash[j]);
                s += oa.get_at(p, keys, hit_keys[j], hit_hash[j]).unwrap_or(0) as u64;
            }
            s
        });
        cell!("openaddr/single/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let p = oa.start(miss_hash[j]);
                s += oa.get_at(p, keys, miss_keys[j], miss_hash[j]).unwrap_or(1) as u64;
            }
            s
        });
        cell!("openaddr/batch32/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            let mut starts = [0usize; 32];
            while j + 32 <= probes {
                for k in 0..32 {
                    starts[k] = oa.start(hit_hash[j + k]);
                    prefetch(unsafe { oa.slot.as_ptr().add(starts[k]) as *const u8 });
                }
                for k in 0..32 {
                    s += oa
                        .get_at(starts[k], keys, hit_keys[j + k], hit_hash[j + k])
                        .unwrap_or(0) as u64;
                }
                j += 32;
            }
            s
        });
        cell!("openaddr/batch32/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            let mut starts = [0usize; 32];
            while j + 32 <= probes {
                for k in 0..32 {
                    starts[k] = oa.start(miss_hash[j + k]);
                    prefetch(unsafe { oa.slot.as_ptr().add(starts[k]) as *const u8 });
                }
                for k in 0..32 {
                    s += oa
                        .get_at(starts[k], keys, miss_keys[j + k], miss_hash[j + k])
                        .unwrap_or(1) as u64;
                }
                j += 32;
            }
            s
        });
        cell!("openaddr/single/hit/cache", |c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let h = hit_hash[j];
                let v = match c.get(h) {
                    Some(v) => v,
                    None => {
                        let v = oa.get_at(oa.start(h), keys, hit_keys[j], h).unwrap_or(0);
                        c.put(h, v);
                        v
                    }
                };
                s += v as u64;
            }
            s
        });
    }

    // ---------------- (b) ptrhash + mapped fingerprint/id records
    if want("ptrhash") {
        let refs: Vec<&[u8]> = (0..n).map(|i| keys.get(i)).collect();
        let t = Instant::now();
        let mph = load_mph(&refs);
        eprintln!("  [mph rebuilt in {:.2}s]", t.elapsed().as_secs_f64());
        let m = map(&dir.join("ptrhash_recs.bin"));
        let recs: &[Rec] = unsafe { std::slice::from_raw_parts(m.as_ptr() as *const Rec, n) };
        cell!("ptrhash/single/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let slot = mph.index(&hit_keys[j]);
                let r = &recs[slot];
                if r.fp == hit_hash[j] {
                    s += r.id as u64;
                }
            }
            s
        });
        cell!("ptrhash/single/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let slot = mph.index(&miss_keys[j]);
                let r = &recs[slot];
                s += if r.fp == miss_hash[j] { r.id as u64 } else { 1 };
            }
            s
        });
        cell!("ptrhash/batch32/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            let mut slots = [0usize; 32];
            while j + 32 <= probes {
                let mut k = 0usize;
                mph.index_stream::<32, _>(&hit_keys[j..j + 32])
                    .for_each(|sl| {
                        if k < 32 {
                            slots[k] = sl;
                        }
                        k += 1;
                    });
                for k in 0..32 {
                    prefetch(unsafe { recs.as_ptr().add(slots[k]) as *const u8 });
                }
                for k in 0..32 {
                    let r = &recs[slots[k]];
                    if r.fp == hit_hash[j + k] {
                        s += r.id as u64;
                    }
                }
                j += 32;
            }
            s
        });
        cell!("ptrhash/batch32/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            let mut slots = [0usize; 32];
            while j + 32 <= probes {
                let mut k = 0usize;
                mph.index_stream::<32, _>(&miss_keys[j..j + 32])
                    .for_each(|sl| {
                        if k < 32 {
                            slots[k] = sl;
                        }
                        k += 1;
                    });
                for k in 0..32 {
                    prefetch(unsafe { recs.as_ptr().add(slots[k]) as *const u8 });
                }
                for k in 0..32 {
                    let r = &recs[slots[k]];
                    s += if r.fp == miss_hash[j + k] {
                        r.id as u64
                    } else {
                        1
                    };
                }
                j += 32;
            }
            s
        });
        cell!("ptrhash/single/hit/cache", |c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let h = hit_hash[j];
                let v = match c.get(h) {
                    Some(v) => v,
                    None => {
                        let r = &recs[mph.index(&hit_keys[j])];
                        let v = if r.fp == h { r.id } else { 0 };
                        c.put(h, v);
                        v
                    }
                };
                s += v as u64;
            }
            s
        });
        // Verify cost: what the fingerprint fast path is buying, measured by
        // adding the mapped-arena string compare back in.
        let am = map(&dir.join("arena.bin"));
        let om = map(&dir.join("arena_off.bin"));
        let aoff: &[u64] = unsafe { std::slice::from_raw_parts(om.as_ptr() as *const u64, n + 1) };
        cell!("ptrhash/single/hit/verify", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let slot = mph.index(&hit_keys[j]);
                let r = &recs[slot];
                if r.fp == hit_hash[j] {
                    let a = aoff[r.id as usize] as usize;
                    let b = aoff[r.id as usize + 1] as usize;
                    if &am[a..b] == hit_keys[j] {
                        s += r.id as u64;
                    }
                }
            }
            s
        });
    }

    // ---------------- (c) front-coded blocks, mapped
    if want("frontcoded") {
        let m = map(&dir.join("frontcoded.bin"));
        let fc = FrontCoded::open(&m);
        assert_eq!(fc.n_keys, n);
        let mut buf = Vec::with_capacity(512);
        cell!("frontcoded/single/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += fc.get(hit_keys[j], &mut buf).unwrap_or(0) as u64;
            }
            s
        });
        cell!("frontcoded/single/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += fc.get(miss_keys[j], &mut buf).unwrap_or(1) as u64;
            }
            s
        });
        cell!("frontcoded/batch32/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut out = Vec::with_capacity(32);
            let mut j = 0;
            while j + 32 <= probes {
                fc.get_batch(&hit_keys[j..j + 32], &mut out, &mut buf);
                for v in &out {
                    s += v.unwrap_or(0) as u64;
                }
                j += 32;
            }
            s
        });
        cell!("frontcoded/batch32/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut out = Vec::with_capacity(32);
            let mut j = 0;
            while j + 32 <= probes {
                fc.get_batch(&miss_keys[j..j + 32], &mut out, &mut buf);
                for v in &out {
                    s += v.unwrap_or(1) as u64;
                }
                j += 32;
            }
            s
        });
        cell!("frontcoded/single/hit/cache", |c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let h = hit_hash[j];
                let v = match c.get(h) {
                    Some(v) => v,
                    None => {
                        let v = fc.get(hit_keys[j], &mut buf).unwrap_or(0);
                        c.put(h, v);
                        v
                    }
                };
                s += v as u64;
            }
            s
        });
    }

    // ---------------- (d) fst, mapped
    if want("fst") {
        let m = map(&dir.join("map.fst"));
        let fm = fst::Map::new(&m[..]).unwrap();
        cell!("fst/single/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += fm.get(hit_keys[j]).unwrap_or(0);
            }
            s
        });
        cell!("fst/single/miss/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                s += fm.get(miss_keys[j]).unwrap_or(1);
            }
            s
        });
        cell!("fst/batch32/hit/nocache", |_c: &mut RepeatCache| {
            let mut s = 0u64;
            let mut j = 0;
            while j + 32 <= probes {
                for k in 0..32 {
                    s += fm.get(hit_keys[j + k]).unwrap_or(0);
                }
                j += 32;
            }
            s
        });
        cell!("fst/single/hit/cache", |c: &mut RepeatCache| {
            let mut s = 0u64;
            for j in 0..probes {
                let h = hit_hash[j];
                let v = match c.get(h) {
                    Some(v) => v,
                    None => {
                        let v = fm.get(hit_keys[j]).unwrap_or(0) as u32;
                        c.put(h, v);
                        v
                    }
                };
                s += v as u64;
            }
            s
        });
    }

    println!("\n== {label}: {probes} probes, {reps} reps, {n} keys");
    println!(
        "{:<34} {:>9} {:>9} {:>9} {:>9}  {}",
        "cell", "ns/lookup", "min", "max", "cache-hit", "checksum"
    );
    for (name, c) in &rows {
        println!(
            "{:<34} {:>9.2} {:>9.2} {:>9.2} {:>8.1}%  {}",
            name,
            c.median(),
            c.lo(),
            c.hi(),
            100.0 * c.cache_hit,
            c.checksum
        );
    }
    println!("peak RSS {:.0} MiB", peak_rss_mib());
}

fn do_query(dir: &Path, probes: usize, reps: usize, zipf: f64, arm: &str) {
    let keys = Keys::read(&dir.join("keys.bin"));
    let n = keys.n();
    let misses = miss_keys(&keys, 1 << 20, 0x5eed_0001);

    // The corpus stream when there is one — the realistic access pattern, and
    // the only one under which the repeat cache means anything. A Zipf stream
    // stands in when there is no corpus stream.
    let sp = dir.join("stream.bin");
    if sp.exists() {
        let full = read_stream(&sp);
        let take = probes.min(full.len());
        run_matrix(
            &keys,
            dir,
            &full[..take],
            &misses,
            reps,
            arm,
            "corpus document-order stream",
        );
    } else {
        let z = zipf_stream(n, probes, zipf, 0x5eed_0002);
        run_matrix(
            &keys,
            dir,
            &z,
            &misses,
            reps,
            arm,
            &format!("zipf s={zipf} stream"),
        );
    }
    // Uniform: no locality at all, so every probe is a genuine random access
    // into the whole structure. The pessimistic bound at this scale.
    let u = uniform_stream(n, probes, 0x5eed_0003);
    run_matrix(&keys, dir, &u, &misses, reps, arm, "uniform stream");
}

fn read_stream(path: &Path) -> Vec<u32> {
    std::fs::read(path)
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// One cold-page-cache measurement. The caller drops the page cache before
/// each invocation; this process maps the arm's file and probes it once, so
/// every probe that misses the cache is a real page fault to disk.
fn do_cold(dir: &Path, probes: usize, arm: &str, zipf: f64) {
    let keys = Keys::read(&dir.join("keys.bin"));
    let n = keys.n();
    let sp = dir.join("stream.bin");
    let stream = if zipf < 0.0 || !sp.exists() {
        uniform_stream(n, probes, 0x5eed_0003)
    } else {
        let full = read_stream(&sp);
        full[..probes.min(full.len())].to_vec()
    };
    let hit_keys: Vec<&[u8]> = stream.iter().map(|&i| keys.get(i as usize)).collect();
    let hit_hash: Vec<u64> = hit_keys.iter().map(|k| hash64(k)).collect();
    match arm {
        "ptrhash" => {
            let refs: Vec<&[u8]> = (0..n).map(|i| keys.get(i)).collect();
            let t = Instant::now();
            let mph = load_mph(&refs);
            let t_mph = t.elapsed().as_secs_f64();
            let m = map(&dir.join("ptrhash_recs.bin"));
            let recs: &[Rec] = unsafe { std::slice::from_raw_parts(m.as_ptr() as *const Rec, n) };
            let t = Instant::now();
            let mut s = 0u64;
            for j in 0..probes {
                let r = &recs[mph.index(&hit_keys[j])];
                if r.fp == hit_hash[j] {
                    s += r.id as u64;
                }
            }
            let e = t.elapsed().as_secs_f64();
            println!(
                "cold ptrhash  {:.1} ns/lookup  (mph rebuild {t_mph:.2}s, {probes} probes, sum {s})",
                e * 1e9 / probes as f64
            );
        }
        "frontcoded" => {
            let m = map(&dir.join("frontcoded.bin"));
            let fc = FrontCoded::open(&m);
            let mut buf = Vec::with_capacity(512);
            let t = Instant::now();
            let mut s = 0u64;
            for j in 0..probes {
                s += fc.get(hit_keys[j], &mut buf).unwrap_or(0) as u64;
            }
            let e = t.elapsed().as_secs_f64();
            println!(
                "cold frontcoded  {:.1} ns/lookup  ({probes} probes, sum {s})",
                e * 1e9 / probes as f64
            );
        }
        "fst" => {
            let m = map(&dir.join("map.fst"));
            let fm = fst::Map::new(&m[..]).unwrap();
            let t = Instant::now();
            let mut s = 0u64;
            for j in 0..probes {
                s += fm.get(hit_keys[j]).unwrap_or(0);
            }
            let e = t.elapsed().as_secs_f64();
            println!(
                "cold fst  {:.1} ns/lookup  ({probes} probes, sum {s})",
                e * 1e9 / probes as f64
            );
        }
        o => panic!("cold arm must be ptrhash|frontcoded|fst, got {o}"),
    }
    println!("peak RSS {:.0} MiB", peak_rss_mib());
}
