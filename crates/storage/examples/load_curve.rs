//! Bulk-load wall time as a function of the tier batch size (HDB-84).
//!
//! ```text
//! cargo run --release -p horndb-storage --example load_curve -- <file.nt|.ttl> <batch> [repeats]
//! ```
//!
//! `<batch>` is triples per `Tier::insert_quad_batch` call. `0` means "one call
//! for the whole document", which is the cheapest the tier can be asked to do
//! and so the floor the batched path is measured against.
//!
//! Two numbers per run, because the tier may leave index work for the first
//! read:
//!
//! * `load` — the loader call alone.
//! * `ready` — `load` plus a first read (`triple_count`), which forces every
//!   partition to finish building. **This is the number to compare**; `load`
//!   on its own is not the cost of a usable store.

use horndb_storage::loader::ntriples::load_ntriples_file;
use horndb_storage::loader::set_load_batch_triples;
use horndb_storage::loader::turtle::load_turtle_file;
use horndb_storage::Store;
use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: load_curve <file.nt|.ttl> <batch-triples, 0 = one call> [repeats]");
        std::process::exit(2);
    }
    let path = Path::new(&args[1]);
    let batch: usize = args[2].parse().expect("batch size");
    let repeats: usize = args
        .get(3)
        .map(|s| s.parse().expect("repeats"))
        .unwrap_or(3);

    // 0 means "never flush early": one insert call for the whole document.
    set_load_batch_triples(if batch == 0 { usize::MAX } else { batch });
    let turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");

    let mut ready_secs = Vec::with_capacity(repeats);
    for i in 0..repeats {
        let store = Store::in_memory();
        let t = Instant::now();
        let stats = if turtle {
            load_turtle_file(&store, path).expect("load turtle")
        } else {
            load_ntriples_file(&store, path).expect("load n-triples")
        };
        let load = t.elapsed().as_secs_f64();
        // Forces every partition to merge its runs.
        let triples = store.triple_count();
        let ready = t.elapsed().as_secs_f64();
        // Measured after the clock stops: the forward map's key bytes, which
        // HDB-95 shrank by substituting a dense id for each datatype IRI.
        let (key_bytes, keys) = store.dictionary().key_bytes();
        println!(
            "run {i}: load {load:.3}s  ready {ready:.3}s  ({triples} triples, {} parsed)  \
             dict {keys} keys, {key_bytes} key bytes ({:.2} B/key)",
            stats.triples,
            key_bytes as f64 / keys.max(1) as f64
        );
        ready_secs.push(ready);
    }

    ready_secs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "batch {batch}: median ready {:.3}s  (min {:.3}s, max {:.3}s)",
        ready_secs[ready_secs.len() / 2],
        ready_secs[0],
        ready_secs[ready_secs.len() - 1]
    );
}
