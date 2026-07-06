---
status: draft
date: 2026-05-24
scope: "SPEC-02 — Storage & Dictionary Encoding"
---

# SPEC-02 — Storage & Dictionary Encoding

## Purpose

Define the on-disk and in-memory representation of RDF triples, the dictionary that maps URIs/literals to internal 64-bit IDs, and the tiered memory hierarchy (HBM / DDR5 / CXL / NVMe). All other subsystems read and write triples through the interface defined here.

## Scope

In scope:
- Dictionary encoding of URIs, blank nodes, plain literals, and typed literals (xsd:int, xsd:decimal, xsd:dateTime, xsd:string with language tag).
- Predicate-partitioned, columnar storage of triples and quads (default-graph + named-graphs).
- Index access paths required by Leapfrog Triejoin (SPO, SOP, PSO, POS, OSP, OPS-orderings — see SPEC-03).
- Tiered memory placement: HBM-resident hot tier, DDR5 warm tier, CXL/NVMe cold tier.
- Compact compressed cold-tier format (HDT-derived).
- Concurrent-read / single-writer MVCC for the hot+warm tiers.

Out of scope:
- The join algorithm itself (SPEC-03).
- Rule-driven writes (SPEC-04, SPEC-06).
- Disk durability beyond crash-consistent checkpoints (no full WAL — see Open Questions).

## Functional requirements

**F1. Dictionary.** Inject URIs/literals and return a stable 64-bit ID. Concurrent reads must be lock-free; writes serialised. Must support reverse lookup (ID → term) for result materialization.

**F2. Term taxonomy.** The 64-bit ID encodes the term kind in its high bits so that hot-path code can distinguish "URI vs literal vs blank node" and "small inline xsd:int vs dictionary-stored" without a dictionary probe.

**F3. Predicate partitioning.** Triples are physically grouped by predicate. Each predicate-partition is a pair of dense Arrow columns `(s_id: u64, o_id: u64)` in some sort order (default: SPO).

**F4. Six orderings on demand.** For predicates flagged as "hot" (configurable; default: any predicate exceeding a triple-count threshold), maintain trie-friendly orderings in all six permutations (SPO, SOP, PSO, POS, OSP, OPS). Cold predicates keep one ordering and materialize others lazily.

**F5. Roaring bitmaps for set operations.** Subject and object ID sets per predicate are exposed as Roaring bitmaps for fast intersection / difference (used by SPEC-05 and SPEC-06).

**F6. Tier API.** A triple-access call returns a triple regardless of which tier it lives on; the executor never reaches across tiers directly. Tier promotion / demotion is observable via metrics but not directly controllable from query plans (Stage 1).

**F7. Named graphs.** Quads supported. Default graph is a named-graph with a reserved sentinel ID.

**F8. Bulk import.** N-Triples, Turtle, N-Quads, HDT inputs. Target throughput ≥1 M triples/sec on a single node for N-Triples (RDFox baseline).

**F9. Snapshot export.** HDT export for the entire store. Used by tests, backup, and cross-engine comparison harnesses.

## Non-functional requirements

**NF1. Memory footprint.** ≤50 bytes/triple in the warm tier (RDFox: 36.9 bytes/triple is the canonical target; we accept a budget of ~35% headroom for the orderings flexibility). Cold tier (HDT): ≤6 bytes/triple amortised.

**NF2. Read throughput.** Sequential scan of a single predicate-partition in HBM at ≥80% of peak HBM bandwidth for the device. On a DDR5 server: ≥80% of peak DDR5 bandwidth per socket.

**NF3. Dictionary lookup.** ID → term: O(1), single cacheline. Term → ID for an interned term: O(1) average, lock-free read.

**NF4. Write amplification on tiering.** A hot-tier eviction triggers at most one rewrite into the warm tier and one rewrite into cold; no read amplification on subsequent reads from the cold tier above 2× over a contiguous HDT-encoded scan.

**NF5. Crash consistency.** A clean restart from disk must produce the last successfully checkpointed state. Lost updates between checkpoints are acceptable in Stage 1; SPEC-06 raises the bar in Stage 2.

## Dependencies

- Apache Arrow (Rust crate `arrow-rs`) for columnar buffers.
- Roaring bitmap crate (`roaring`).
- HDT library (Rust port `hdt-rs` or new wrapping of the C++ reference).
- `io_uring` for async NVMe I/O on Linux.
- CXL access via standard `mmap` on `/dev/dax` (no custom kernel module).

## Acceptance criteria

1. Bulk-import LUBM-100 (~13 M triples) from N-Triples in ≤30 s on a reference workstation (single AMD EPYC 9354, 12 channels DDR5-4800).
2. Bulk-import LUBM-8000 (1.1B triples) on the reference workstation in ≤30 minutes.
3. Memory footprint on LUBM-8000 fully warm (no cold tier) ≤55 GB (50 bytes/triple budget).
4. Sequential scan of the `rdf:type` predicate-partition on LUBM-8000 reaches ≥80% of measured `STREAM Triad` bandwidth for the device.
5. HDT round-trip (import → store → export → re-import) produces an isomorphic store under blank-node renaming.
6. All six index orderings are queryable for the top-10 predicates (by triple count) on LUBM-8000.

## Risks and open questions

- **Inline-literal encoding loses precision for `xsd:double`.** Decide whether to inline only `xsd:int` (i32 fits) or use a tagged 60-bit space. Default: inline `xsd:int` and small `xsd:string` (≤6 bytes UTF-8) only; everything else hits the dictionary.
- **Dictionary on disk for cold tier.** A 10B-triple store can produce a multi-GB dictionary. Defer the persistent-dictionary design (consider Marisa-trie / FST / Plan9 sorted-string-table) to Stage 2.
- **MVCC vs copy-on-write snapshots.** Stage 1 uses copy-on-write snapshots (read transaction = stable snapshot ID). True MVCC with per-tuple visibility is Stage 2 and intersects SPEC-06.
- **Write-ahead log.** None in Stage 1 — crash recovery rolls back to last checkpoint. Stage 2 may add a per-predicate-partition WAL.
- **NUMA / multi-socket.** Reference workstation is single-socket. Multi-socket NUMA-aware placement is Stage 3 (SPEC-09).
