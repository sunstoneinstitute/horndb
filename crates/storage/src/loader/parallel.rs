//! Parallel chunked parsing for the slice-based bulk loaders (HDB-83).
//!
//! `oxttl` can cut a document slice into N independently parseable chunks
//! (`split_slice_for_parallel_parsing`). This module turns those chunks into a
//! **parse-parallel, intern-serial** pipeline:
//!
//! * one OS thread per chunk runs the parser and pushes fixed-size batches of
//!   parsed items down a bounded channel;
//! * the caller's thread drains the chunk channels **in document order** and
//!   does all interning and tier insertion itself.
//!
//! Keeping the intern step on one thread in document order is deliberate: it
//! makes the parallel path produce byte-identical store contents to the serial
//! path — same triples, same dictionary, same term ids — so the two are
//! interchangeable and the differential tests can compare them exactly.
//! Interning on the chunk threads instead would be a little faster in
//! isolation (it scales ~3.9× on 16 cores, so the dictionary's reverse-map
//! write lock is *not* the bottleneck it was assumed to be) but it would make
//! term ids depend on thread scheduling. Since HDB-96 the serial intern is the
//! largest phase of a load — see [`load_threads`] for the sweep that made the
//! parse threads the default and left interning where it is.
//!
//! The bounded channels cap memory: at most [`load_buffer_triples`] parsed
//! items are in flight across all chunks (see [`channel_depth`]).

use crate::error::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;

/// Items per batch handed from a parse thread to the consumer.
const BATCH: usize = 8_192;

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

/// Parse-thread count for the slice loaders. **Defaults to `auto`** — see
/// [`AUTO_THREAD_CAP`] for what `auto` resolves to.
///
/// `HORNDB_LOAD_THREADS=<n>` sets it explicitly and is not capped;
/// `HORNDB_LOAD_THREADS=1` restores the pre-HDB-96 serial behaviour.
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
/// The cost of the default is memory: peak RSS rises 78% on Turtle
/// (2,207 -> 3,851 MiB) and 57% on N-Triples. Most of it is the 8M-triple
/// in-flight parse budget, which a one-chunk load never allocates. Lower it
/// with [`load_buffer_triples`], or set `HORNDB_LOAD_THREADS=1` to get the old
/// footprint back.
///
/// Turtle *files* are not affected: `load_turtle_file` needs
/// `HORNDB_PARALLEL_TURTLE=1` as well, because splitting Turtle carries a
/// soundness caveat the line-based formats do not.
pub fn load_threads() -> usize {
    match std::env::var("HORNDB_LOAD_THREADS").as_deref() {
        Ok("auto") => auto_load_threads(),
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .unwrap_or_else(auto_load_threads),
        Err(_) => auto_load_threads(),
    }
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
    mut sink: F,
) -> Result<()>
where
    T: Send,
    F: FnMut(Vec<T>) -> Result<()>,
{
    // One chunk means there is nothing to overlap. Run it inline rather than
    // paying for a thread and a channel — and, more to the point, so terms are
    // freed on the thread that allocated them.
    if chunks.len() == 1 {
        let mut batch = Vec::with_capacity(BATCH);
        for item in chunks.into_iter().next().expect("one chunk") {
            batch.push(item?);
            if batch.len() >= BATCH {
                sink(std::mem::replace(&mut batch, Vec::with_capacity(BATCH)))?;
            }
        }
        if !batch.is_empty() {
            sink(batch)?;
        }
        return Ok(());
    }

    let depth = channel_depth(chunks.len());
    let mut senders = Vec::with_capacity(chunks.len());
    let mut receivers = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        let (tx, rx) = sync_channel::<Result<Vec<T>>>(depth);
        senders.push(tx);
        receivers.push(rx);
    }

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
                                if tx.send(Ok(full)).is_err() {
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
                    let _ = tx.send(Ok(batch));
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
}
