//! Parallel chunked parsing for the slice-based bulk loaders (HDB-83).
//!
//! `oxttl` can cut a document slice into N independently parseable chunks
//! (`split_slice_for_parallel_parsing`). This module turns those chunks into a
//! **parse-parallel, intern-serial** pipeline:
//!
//! * one OS thread per chunk runs the parser, **probes** each term against the
//!   dictionary read-only (from [`MIN_PROBE_CHUNKS`] chunks up), and pushes
//!   fixed-size batches of parsed items down a bounded channel;
//! * the caller's thread drains the chunk channels **in document order** and
//!   allocates ids for whatever the probes did not resolve, then does the tier
//!   insertion.
//!
//! Keeping *id allocation* on one thread in document order is deliberate: it
//! makes the parallel path produce byte-identical store contents to the serial
//! path — same triples, same dictionary, same term ids — so the two are
//! interchangeable and the differential tests can compare them exactly.
//! Allocating ids on the chunk threads instead would be a little faster in
//! isolation (it scales ~3.9× on 16 cores, so the dictionary's reverse-map
//! write lock is *not* the bottleneck it was assumed to be) but it would make
//! term ids depend on thread scheduling.
//!
//! **The probe is the way round that (HDB-106).** After HDB-96 made parse
//! threads the default, interning became the largest phase of a load — 56–61%
//! of trainmarks xlarge — while the parse threads sat idle waiting on the
//! channel. A corpus has far more term *occurrences* than distinct terms
//! (trainmarks xlarge: 9,995,000 triples over 1,919,818 distinct terms, ~15.6
//! occurrences each), so the overwhelming majority of intern calls are lookups
//! that find an existing id. Those lookups allocate nothing, so they can run
//! anywhere; only the misses have to be serialised. The parse threads do the
//! lookups, the consumer allocates. See [`crate::loader::Probed`] and
//! [`crate::dictionary::Dictionary::get`] for why racing the consumer is safe.
//!
//! It is **not** unconditional. Moving the lookups pays only where the parse
//! threads have spare capacity to absorb them, which is from 4 chunks up; at 2
//! it is a 4–5% loss even though the probe resolves *more* there.
//! [`MIN_PROBE_CHUNKS`] carries the measurements and the reasoning.
//!
//! The bounded channels cap memory: at most [`load_buffer_triples`] parsed
//! items are in flight across all chunks (see [`channel_depth`]).

use crate::error::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;

/// Items per batch handed from a parse thread to the consumer, and the
/// granularity every load-phase clock on this path is taken at.
pub(crate) const BATCH: usize = 8_192;

/// Default for [`load_buffer_triples`]: parsed triples that may sit in the
/// chunk channels at once, summed over every chunk.
///
/// The consumer drains the chunk receivers strictly in document order, so a
/// parse thread stops as soon as its own channel is full and cannot restart
/// until the drain reaches it. The buffer is therefore not an optimisation
/// detail — it is how much of the document the parse is allowed to run in
/// parallel. Too small and every producer but one is parked, which is what
/// made the "parallel" parse run at one-thread speed (HDB-86).
///
/// Measured on hornbench, trainmarks xlarge (9,995,000 triples), 16 threads,
/// Turtle — full table, conditions, and the memory trade in
/// `docs/benchmarks.md`:
///
/// | budget | `parse` | vs 262 k | peak RSS |
/// |---|---|---|---|
/// | 262,144 (the old 2-batches-per-chunk bound) | 8.677s | — | 13,719 MiB |
/// | 4,194,304 | 5.506s | 1.58x | 13,675 MiB |
/// | **8,388,608 (this default)** | **2.381s** | **3.64x** | 14,424 MiB (+5.1%) |
/// | 16,777,216 | 1.743s | 4.98x | 14,893 MiB (+8.6%) |
///
/// 8M is the knee: doubling it again buys 3% more end-to-end for another 3.5
/// points of peak memory. Worst case it holds ~1 GiB of parsed terms, and only
/// for a document large enough to fill it.
///
/// The budget is a **total**, not per chunk, so peak in-flight memory does not
/// grow with `HORNDB_LOAD_THREADS`; more threads split the same budget more
/// ways. It is still the bulk of what the threaded default costs in memory
/// (HDB-96: peak RSS 2,207 -> 3,851 MiB at trainmarks xlarge Turtle). That is what the old per-chunk constant got wrong: its cost and its
/// benefit both scaled with the thread count, so neither was visible in the
/// constant itself.
///
/// A one-chunk load never allocates it at all — a single chunk skips the
/// channel entirely — so `HORNDB_LOAD_THREADS=1` is also how to get the
/// pre-HDB-96 memory footprint back.
pub const DEFAULT_LOAD_BUFFER_TRIPLES: usize = 8 << 20;

/// Resolved once, then cached: `HORNDB_LOAD_BUFFER_TRIPLES` if set and
/// parseable, else [`DEFAULT_LOAD_BUFFER_TRIPLES`]. `0` means "not yet read".
static LOAD_BUFFER_TRIPLES: AtomicUsize = AtomicUsize::new(0);

/// Total parsed triples allowed in the chunk channels at once.
///
/// Set it with `HORNDB_LOAD_BUFFER_TRIPLES=<n>`, or from code with
/// [`set_load_buffer_triples`]. Raising it buys parse overlap and costs
/// transient memory; the trade is measured in `docs/benchmarks.md`.
pub fn load_buffer_triples() -> usize {
    match LOAD_BUFFER_TRIPLES.load(Ordering::Relaxed) {
        0 => {
            let v = std::env::var("HORNDB_LOAD_BUFFER_TRIPLES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n >= 1)
                .unwrap_or(DEFAULT_LOAD_BUFFER_TRIPLES);
            LOAD_BUFFER_TRIPLES.store(v, Ordering::Relaxed);
            v
        }
        v => v,
    }
}

/// Override [`load_buffer_triples`] for this process, ignoring the environment.
///
/// The buffer size cannot change what a load produces — only how much of the
/// parse overlaps — so this is safe to move around. The determinism tests use
/// it to prove exactly that.
pub fn set_load_buffer_triples(triples: usize) {
    LOAD_BUFFER_TRIPLES.store(triples.max(1), Ordering::Relaxed);
}

/// Batches one parse thread may run ahead of the consumer, for a run split
/// into `chunks` chunks: the total budget shared out evenly, at least one
/// batch so every producer can always make progress.
///
/// Peak in-flight items are a little above the budget — each producer also
/// holds a partly-filled batch, and the consumer holds one — so the true cap
/// is `chunks * (depth + 2) * BATCH`.
fn channel_depth(chunks: usize) -> usize {
    (load_buffer_triples() / (chunks.max(1) * BATCH)).max(1)
}

/// Fewest parse chunks that make the dictionary probe worth doing.
///
/// The probe moves ~1.5s of lookup work (trainmarks xlarge) off the consumer
/// and **onto the parse threads**. That is a win only where the parse threads
/// have idle capacity to absorb it, and below 4 chunks they do not. Measured on
/// hornbench, base `60b300a` vs the probe, median of 3 interleaved reps, full
/// table in `docs/benchmarks.md`:
///
/// | chunks | Turtle wall | N-Triples wall |
/// |---|---|---|
/// | 1 | +0.6% | −3.7% |
/// | **2** | **+4.1%** | **+5.4%** |
/// | 4 | −4.9% | −5.2% |
/// | 8 (the `auto` cap) | −9.0% | −7.9% |
///
/// At 2 chunks the probe *works better* than at 8 — `intern` falls to 1.79s,
/// its best figure at any chunk count, because two slow producers cannot run as
/// far ahead of the consumer as eight fast ones, so the probe sees a warmer
/// dictionary. It loses anyway: the parse is already close to the critical path
/// there (2.76s of a 7.47s Turtle load), so work added to it lands on the wall
/// clock roughly one-for-one, while the `intern` saving comes off the consumer,
/// which is not the constraint.
///
/// `HORNDB_LOAD_THREADS` defaults to `auto` = `available_parallelism()` capped
/// at 8, so a 2-core VM, container or CI runner reaches this path *by default*.
/// Regressing the default on the smallest shipped configuration is not
/// something a documentation note can buy off — hence the gate.
///
/// **4 because that is the line the measurements draw**, not a tuned threshold:
/// 1, 2, 4 and 8 were measured, the sign flips between 2 and 4, and nothing was
/// measured in between to justify anything finer. Below the gate the loaders
/// take the pre-HDB-106 path exactly (`Batch::Raw`), which costs nothing.
pub(crate) const MIN_PROBE_CHUNKS: usize = 4;

/// Should the parse threads probe the dictionary for a run split into `chunks`
/// chunks? See [`MIN_PROBE_CHUNKS`].
///
/// Keyed on the **actual chunk count**, not on `HORNDB_LOAD_THREADS`: `oxttl`
/// applies its own 16 KiB-per-chunk floor and may hand back fewer chunks than
/// the thread count asked for, and a Turtle document `turtle_split_is_safe`
/// rejects comes back as one chunk whatever the setting. The chunk count is
/// what decides how much parse capacity there actually is.
pub(crate) fn should_probe(chunks: usize) -> bool {
    chunks >= MIN_PROBE_CHUNKS
}

/// Below this document size the `load_*_slice` entry points parse on one
/// thread: spawning threads, and (for Turtle) the chunker's prefix prescan and
/// per-boundary probe, cost more than the parse itself. `oxttl` applies its own
/// 16 KiB-per-chunk floor on top, so it may still hand back fewer chunks than
/// the thread count.
///
/// The `for_each_*_batch` primitives do **not** apply this floor — the caller
/// passes the thread count it wants, and `1` means serial. That is what the
/// differential tests use to exercise the split on small documents.
pub(crate) const MIN_PARALLEL_BYTES: usize = 1 << 20;

/// Ceiling on what `auto` resolves to, and therefore on the shipped default.
///
/// The measured knee (HDB-96, table in [`load_threads`]): the fourth and
/// eighth threads still pay, the ninth onwards does not. By 8 threads `parse`
/// is 14% of a Turtle load and 4% of an N-Triples one, so even driving it to
/// zero could not buy another 15% — what is left is interning and the tier,
/// both of which run on the calling thread by design.
///
/// It is also a guard on a default nobody sets: without a cap, a 64-core host
/// would spawn 64 parse threads for a leg that stopped scaling at 8, and pay
/// for all of them in scheduler pressure and per-thread parser state. An
/// explicit `HORNDB_LOAD_THREADS=<n>` is not capped — that is the escape hatch,
/// and how the sweep was taken.
const AUTO_THREAD_CAP: usize = 8;

/// Default for [`max_slice_bytes`]: the largest document a `load_*_file` entry
/// point will read into memory to parse in parallel.
///
/// **This ceiling is why the threaded default is safe.** The parallel path
/// needs one contiguous slice, so `load_ntriples_file` / `load_nquads_file`
/// (and `load_turtle_file` under `HORNDB_PARALLEL_TURTLE=1`) take it by reading
/// the whole document into memory, where the streaming path holds only a 1 MiB
/// `BufReader`. Before HDB-96 that branch was unreachable by default — it
/// needed `threads > 1`, which nobody had — so the cost never shipped. Making
/// threads the default makes it the default too, and file size is unbounded:
/// without a ceiling, a document larger than RAM would load before HDB-96 and
/// OOM after it.
///
/// Above the ceiling the loaders fall back to the streaming reader on one
/// thread — exactly what they did before HDB-96 — so a large file gets slower,
/// never fatal.
///
/// 2 GiB for two reasons, neither of which is the document-to-store ratio —
/// that is roughly scale-invariant for RDF and does not change at any
/// threshold:
///
/// * **It bounds the transient absolutely.** Whatever the corpus, the extra
///   memory a threaded file load can take over a streaming one is at most this
///   ceiling plus the parse budget — worst case about +3.5 GiB. Any host with
///   room for the store the load is building has room for that; without a
///   ceiling the term is unbounded, which is the actual hazard.
/// * **What it forgoes is small.** At the shipped 8 threads `parse` is 14% of a
///   Turtle load and 3.6% of an N-Triples one (HDB-96), so a document that trips
///   the ceiling gives up at most that much and keeps the streaming loader's
///   flat footprint.
///
/// For scale: trainmarks xlarge is a 1.17 GB document and a ~1.7 GiB store, so
/// the ceiling sits just above a corpus of that size — comfortably inside the
/// bound, not tuned to it.
pub const DEFAULT_MAX_SLICE_BYTES: usize = 2 << 30;

/// Resolved once, then cached. `0` means "not yet read".
static MAX_SLICE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Largest document a `load_*_file` entry point will read into memory in order
/// to parse it in parallel. Set with `HORNDB_LOAD_MAX_SLICE_BYTES=<n>`.
///
/// It does not bound [`load_turtle_slice`](crate::loader::turtle::load_turtle_slice)
/// and friends: a caller that already holds the bytes has already paid for
/// them, and this ceiling exists to stop the *file* loaders from allocating a
/// copy the caller never asked for.
///
/// A malformed or `0` value resolves to `1` — "never read a file whole" — for
/// the same reason a malformed `HORNDB_LOAD_THREADS` resolves to 1: a setting
/// nobody can read should cost time, not memory.
pub fn max_slice_bytes() -> usize {
    match MAX_SLICE_BYTES.load(Ordering::Relaxed) {
        0 => {
            let v = match std::env::var("HORNDB_LOAD_MAX_SLICE_BYTES") {
                Ok(s) => parse_max_slice_bytes(&s),
                Err(_) => DEFAULT_MAX_SLICE_BYTES,
            };
            MAX_SLICE_BYTES.store(v, Ordering::Relaxed);
            v
        }
        v => v,
    }
}

/// Parse an explicit `HORNDB_LOAD_MAX_SLICE_BYTES`.
///
/// Same convention as [`parse_thread_count`]: a malformed value falls back to
/// the **restrictive** setting, not the permissive one. Here that is `1` — no
/// document can be both at least [`MIN_PARALLEL_BYTES`] and at most one byte,
/// so `1` reads as "never read a file whole". `0` means the same and is stored
/// as `1`, because `0` is the not-yet-resolved sentinel for the cache.
///
/// Restrictive means the opposite direction for the two variables — fewer
/// threads, a lower ceiling — but the same rule: a setting nobody can read
/// costs time, never memory.
fn parse_max_slice_bytes(v: &str) -> usize {
    v.parse::<usize>().unwrap_or(1).max(1)
}

/// Whether a `load_*_file` entry point should read `len` bytes into memory and
/// parse them on `threads` threads, rather than streaming on one.
///
/// Three conditions, all of which have to hold: more than one thread to use, a
/// document big enough that splitting beats the setup cost, and one small
/// enough that holding it whole is affordable. Pulled out of the three file
/// loaders so the policy is stated and tested once.
pub(crate) fn should_read_whole_file(len: u64, threads: usize) -> bool {
    threads > 1 && len >= MIN_PARALLEL_BYTES as u64 && len <= max_slice_bytes() as u64
}

/// Parse-thread count for the slice loaders. **Defaults to `auto`** — see
/// [`AUTO_THREAD_CAP`] for what `auto` resolves to.
///
/// `HORNDB_LOAD_THREADS=<n>` sets it explicitly and is not capped;
/// `HORNDB_LOAD_THREADS=1` restores the pre-HDB-96 serial behaviour. A value
/// that does not parse, or is `0`, also resolves to 1 — a malformed setting
/// falls back to the cheap behaviour, not the expensive one.
///
/// Measured on hornbench (Ryzen 7 7700, 8 cores / 16 threads, 124 GB, Debian
/// 6.12, rustc 1.90.0), commit `c6da644`, trainmarks xlarge (9,995,000
/// triples) loaded into a fresh `Store` through `load_turtle_slice` /
/// `load_ntriples_slice` plus a first read to force HDB-84's deferred merge.
/// Median of 3 interleaved runs; full table and phase split in
/// `docs/benchmarks.md`:
///
/// | threads | Turtle wall | N-Triples wall | `parse` (ttl) | peak RSS (ttl) |
/// |---|---|---|---|---|
/// | 1 | 12.926s | 9.672s | 8.479s | 2,207 MiB |
/// | 2 | 7.388s | 5.926s | 2.613s | 3,210 MiB |
/// | 4 | 6.255s | 5.199s | 1.535s | 3,698 MiB |
/// | **8 (the cap)** | **5.581s** | **4.903s** | **0.780s** | 3,851 MiB |
/// | 16 | 5.499s | 4.956s | 0.777s | 3,915 MiB |
///
/// **HDB-83's reason for the serial default no longer holds.** It measured a
/// real `Store` load getting *slower* with threads (40.1s -> 46.6s) because
/// `Tier::insert_quad_batch` had to free terms allocated on the parse threads
/// while rebuilding every partition it touched. HDB-84 replaced that rebuild
/// with an appended run, and the tier phases are now flat in the thread count:
/// `group` + `build` + `merge_runs` total 1.65s at 1 thread and 1.70s at 16.
/// What is left of the cross-thread free cost shows up in interning, which
/// rises 2.78s -> 3.05s (+10%) from 1 to 2 threads and is flat above that.
///
/// The cost of the default is memory, in two parts. The measured one: peak RSS
/// rises 78% on Turtle (2,207 -> 3,851 MiB) and 57% on N-Triples, almost all of
/// it the 8M-triple in-flight parse budget, which a one-chunk load never
/// allocates. That part is absolute — lower it with [`load_buffer_triples`].
///
/// The second part is not in the table and does scale with the document: the
/// `load_*_file` entry points reach the parallel path by reading the whole file
/// into memory, a branch that was unreachable while the default was 1. See
/// [`DEFAULT_MAX_SLICE_BYTES`] for the ceiling that bounds it, and
/// `docs/benchmarks.md` for the accounting.
///
/// `HORNDB_LOAD_THREADS=1` restores the old time and both parts of the old
/// footprint.
///
/// Turtle *files* are not affected: `load_turtle_file` needs
/// `HORNDB_PARALLEL_TURTLE=1` as well, because splitting Turtle carries a
/// soundness caveat the line-based formats do not.
pub fn load_threads() -> usize {
    match std::env::var("HORNDB_LOAD_THREADS") {
        Ok(v) => parse_thread_count(&v),
        Err(_) => auto_load_threads(),
    }
}

/// Parse an explicit `HORNDB_LOAD_THREADS`.
///
/// An unparseable or zero value falls back to **1**, not to `auto`: someone who
/// wrote `=0` meaning "off" must not get the most expensive setting and the
/// +1.5 GiB that comes with it.
fn parse_thread_count(v: &str) -> usize {
    if v == "auto" {
        return auto_load_threads();
    }
    v.parse::<usize>().ok().filter(|n| *n >= 1).unwrap_or(1)
}

/// `available_parallelism()` clamped to [`AUTO_THREAD_CAP`], 1 if the platform
/// will not say.
fn auto_load_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(AUTO_THREAD_CAP))
        .unwrap_or(1)
}

/// Run `chunks` on one thread each and feed `sink` their batches in document
/// order (chunk 0 fully, then chunk 1, …). `sink` runs on the calling thread.
///
/// The first error — from a parser or from `sink` — aborts the run: dropping
/// the remaining receivers makes the still-running parse threads fail their
/// next `send` and exit, and the scope joins them before returning.
pub(crate) fn parse_chunks_ordered<T, F>(
    chunks: Vec<Box<dyn Iterator<Item = Result<T>> + Send + '_>>,
    sink: F,
) -> Result<()>
where
    T: Send,
    F: FnMut(Vec<T>) -> Result<()>,
{
    parse_chunks_mapped(chunks, |batch| batch, sink)
}

/// [`parse_chunks_ordered`], with `map` applied to each **whole batch** on the
/// parse thread that produced it, before the batch is handed down the channel.
///
/// This is where the loaders put the dictionary probe (HDB-106): the parse
/// threads are idle most of a threaded load — at 8 threads `parse` is 14% of a
/// Turtle load — while the consumer is saturated by interning, so read-only
/// work the consumer would otherwise do serially is close to free here. `map`
/// must not allocate term ids; see [`crate::loader::Probed`] for the
/// determinism argument.
///
/// Per batch and not per item so the map can decide *once*, for 8,192 rows,
/// whether to probe at all — a single-chunk parse hands the rows through
/// untouched and pays nothing, in time or in bytes, for a probe it cannot use.
pub(crate) fn parse_chunks_mapped<T, U, M, F>(
    chunks: Vec<Box<dyn Iterator<Item = Result<T>> + Send + '_>>,
    map: M,
    mut sink: F,
) -> Result<()>
where
    T: Send,
    U: Send,
    M: Fn(Vec<T>) -> U + Sync,
    F: FnMut(U) -> Result<()>,
{
    // One chunk means there is nothing to overlap. Run it inline rather than
    // paying for a thread and a channel — and, more to the point, so terms are
    // freed on the thread that allocated them.
    if chunks.len() == 1 {
        let mut batch = Vec::with_capacity(BATCH);
        for item in chunks.into_iter().next().expect("one chunk") {
            batch.push(item?);
            if batch.len() >= BATCH {
                sink(map(std::mem::replace(
                    &mut batch,
                    Vec::with_capacity(BATCH),
                )))?;
            }
        }
        if !batch.is_empty() {
            sink(map(batch))?;
        }
        return Ok(());
    }

    let depth = channel_depth(chunks.len());
    let mut senders = Vec::with_capacity(chunks.len());
    let mut receivers = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        let (tx, rx) = sync_channel::<Result<U>>(depth);
        senders.push(tx);
        receivers.push(rx);
    }

    let map = &map;
    std::thread::scope(|scope| -> Result<()> {
        for (chunk, tx) in chunks.into_iter().zip(senders) {
            scope.spawn(move || {
                let mut batch = Vec::with_capacity(BATCH);
                for item in chunk {
                    match item {
                        Ok(v) => {
                            batch.push(v);
                            if batch.len() >= BATCH {
                                let full = std::mem::replace(&mut batch, Vec::with_capacity(BATCH));
                                if tx.send(Ok(map(full))).is_err() {
                                    return; // consumer gave up
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
                if !batch.is_empty() {
                    let _ = tx.send(Ok(map(batch)));
                }
            });
        }

        for rx in receivers {
            for msg in rx {
                sink(msg?)?;
            }
        }
        Ok(())
    })
}

/// Parse-thread count for a `load_*_slice` call on a document of `len` bytes:
/// [`load_threads`] above the size floor, 1 below it.
pub(crate) fn slice_threads(len: usize) -> usize {
    if len >= MIN_PARALLEL_BYTES {
        load_threads()
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe gate is a throughput switch with a measured boundary, so pin
    /// the boundary: 2 and 3 chunks are the cells where the probe measured as a
    /// loss, 4 is where it turned into a win. A change here is a claim about
    /// numbers, and needs new ones (`docs/benchmarks.md`).
    ///
    /// `parallel_loader.rs::both_sides_of_the_probe_gate_produce_the_same_store`
    /// covers the other half: whichever side the gate picks, the store is the
    /// same.
    #[test]
    fn the_probe_gate_turns_on_at_four_chunks() {
        assert_eq!(MIN_PROBE_CHUNKS, 4);
        for chunks in [0usize, 1, 2, 3] {
            assert!(!should_probe(chunks), "{chunks} chunks must not probe");
        }
        for chunks in [4usize, 8, 16, 64] {
            assert!(should_probe(chunks), "{chunks} chunks must probe");
        }
    }

    /// The shipped default resolves inside the cap on every host. Pins
    /// [`AUTO_THREAD_CAP`] against an accidental "just use every core".
    #[test]
    fn auto_load_threads_stays_inside_the_cap() {
        let n = auto_load_threads();
        assert!(
            (1..=AUTO_THREAD_CAP).contains(&n),
            "auto resolved to {n}, outside 1..={AUTO_THREAD_CAP}"
        );
    }

    /// The whole-file read is gated on all three conditions, not just the two
    /// it had before HDB-96. The ceiling is the one that keeps the threaded
    /// default from turning an arbitrarily large file into an OOM: past it the
    /// caller falls back to the streaming reader.
    #[test]
    fn should_read_whole_file_is_bounded_at_both_ends() {
        let max = max_slice_bytes() as u64;
        let min = MIN_PARALLEL_BYTES as u64;

        // One thread never reads the file whole, at any size.
        assert!(!should_read_whole_file(min, 1));
        assert!(!should_read_whole_file(max, 1));

        // Below the floor the split costs more than it saves.
        assert!(!should_read_whole_file(min - 1, 8));

        // In between, it does.
        assert!(should_read_whole_file(min, 8));
        assert!(should_read_whole_file(max, 8));

        // Above the ceiling it falls back to streaming rather than allocating
        // a copy of the document.
        assert!(!should_read_whole_file(max + 1, 8));
        assert!(!should_read_whole_file(u64::MAX, 8));
    }

    /// Both knobs resolve a malformed value to their restrictive setting, and
    /// they agree on which values count as malformed. Driven through the real
    /// parse functions, not a copy of them, and off the environment, so the
    /// test stays process-local.
    #[test]
    fn a_malformed_setting_resolves_to_the_restrictive_value() {
        for v in ["0", "", "auto ", "-1", "eight", "1.5", " 4"] {
            assert_eq!(parse_thread_count(v), 1, "{v:?} threads");
            assert_eq!(parse_max_slice_bytes(v), 1, "{v:?} max slice bytes");
        }
        // A well-formed value is still honoured on both.
        assert_eq!(parse_thread_count("4"), 4);
        assert_eq!(parse_max_slice_bytes("4096"), 4096);
    }

    /// A ceiling of 1 byte is what a malformed value resolves to, and it has to
    /// mean "never read a file whole" — no document can clear the 1 MiB floor
    /// and still fit under one byte.
    #[test]
    fn a_one_byte_ceiling_disables_the_whole_file_read() {
        assert!(MIN_PARALLEL_BYTES as u64 > parse_max_slice_bytes("garbage") as u64);
    }
}
