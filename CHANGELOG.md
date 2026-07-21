# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-21

- Added: hermetic owl:imports resolution — closes the RL-reachable OWL 2 RL conformance gap.
- Added: datatype value-space intersection narrows rdfs:range for tighter type inference.
- Added: streaming SPARQL SELECT results over HTTP, sent chunk-by-chunk; ASK answers from the first chunk.
- Added: query optimizer — logical IR, heuristic rewrites (filter/projection pushdown), and a Characteristic-Sets cardinality estimator.
- Added: incremental retraction — rule and closure deletions are now maintained incrementally, not recomputed from scratch.
- Added: per-tuple MVCC visibility and a delete path in storage.
- Added: Prometheus metrics across the owlrl, incremental, ml, wcoj, and sparql subsystems, plus the selected SIMD ISA per kernel.
- Improved: SPARQL runtime is now fully streaming, replacing the materializing evaluator — lower memory on large results.
- Improved: COUNT-over-BGP and grouped-count pushdown answer aggregate queries without materializing bindings.
- Improved: faster SPARQL joins via FxHash hot tables and canonicalizing join keys.
- Improved: SIMD-vectorized set intersection with per-host kernel calibration (incl. NEON on Apple Silicon).
- Improved: faster WCOJ per-tuple hot path via galloping descent and bulk leaf materialization.
