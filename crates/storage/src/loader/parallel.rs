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
//! The bounded channels cap memory: at most `CHANNEL_DEPTH * BATCH` parsed
//! items are in flight per chunk.

use crate::error::Result;
use std::sync::mpsc::sync_channel;

/// Items per batch handed from a parse thread to the consumer.
const BATCH: usize = 8_192;

/// Batches a parse thread may run ahead of the consumer, per chunk.
const CHANNEL_DEPTH: usize = 2;

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
/// The default is 1 because chunking does not currently pay. Measured on
/// hornbench (16 cores, trainmarks xlarge, ~10M triples, N-Triples):
///
/// | phase | 1 thread | 4 | 16 |
/// |---|---|---|---|
/// | parse only | 5.41s | 1.65s | 0.67s |
/// | parse + intern | 8.08s | 2.96s | 2.06s |
/// | full `Store` load | 40.1s | 43.4s | 46.6s |
///
/// Parsing scales 8x and interning scales 4x, but both together are ~20% of a
/// bulk load: the rest is `Tier::insert_quad_batch` building the six trie
/// orderings per predicate, which is serial and unaffected. Making the parse
/// concurrent therefore buys single-digit percent at best, and the extra
/// threads contend enough to lose it again.
///
/// The machinery stays because it is correct, tested, and free at
/// `threads == 1` — re-measure with `HORNDB_LOAD_THREADS=auto` once the index
/// build stops dominating. See `docs/benchmarks.md`.
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

    let mut senders = Vec::with_capacity(chunks.len());
    let mut receivers = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        let (tx, rx) = sync_channel::<Result<Vec<T>>>(CHANNEL_DEPTH);
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
