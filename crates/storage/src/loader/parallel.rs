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
//! term ids depend on thread scheduling, and it does not help the number that
//! matters — see [`load_threads`] for why the whole thing is off by default.
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
/// ways. That is what the old per-chunk constant got wrong: its cost and its
/// benefit both scaled with the thread count, so neither was visible in the
/// constant itself.
///
/// It costs nothing at the shipped default of one parse thread — a single
/// chunk skips the channel entirely.
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

/// Parse-thread count for the slice loaders. **Defaults to 1 — serial.**
///
/// `HORNDB_LOAD_THREADS=<n>` sets it explicitly; `HORNDB_LOAD_THREADS=auto`
/// uses [`std::thread::available_parallelism`].
///
/// The default is 1 because the *whole load* has not been shown to get faster
/// with more parse threads, not because the parse does not scale — since
/// HDB-94 it does. Measured on hornbench (16 cores, trainmarks xlarge,
/// 9,995,000 triples, Turtle, default buffer budget):
///
/// | | 1 thread | 4 | 16 |
/// |---|---|---|---|
/// | `parse` phase | 9.126s | 3.738s | 2.381s |
/// | parse -> `Vec` -> `HornBackend` insert (bench driver) | 21.920s | 16.208s | 15.002s |
///
/// What is still unmeasured is the same sweep against a real `Store` load.
/// HDB-83 measured that path getting *slower* with threads (40.1s -> 46.6s),
/// because the serial `Tier::insert_quad_batch` has to free terms allocated on
/// the parse threads while those threads keep allocating. Two things have
/// changed under it since — snmalloc (HDB-86 E1) and the buffer budget — so
/// the number needs re-taking before the default flips. See
/// `docs/benchmarks.md`.
///
pub fn load_threads() -> usize {
    match std::env::var("HORNDB_LOAD_THREADS").as_deref() {
        Ok("auto") => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        Ok(v) => v.parse::<usize>().ok().filter(|n| *n >= 1).unwrap_or(1),
        Err(_) => 1,
    }
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
