# Integration Notes for `horndb-closure`

## Build: vendored SuiteSparse:GraphBLAS

`horndb-closure` builds **SuiteSparse:GraphBLAS from a vendored git
submodule** (`vendor/GraphBLAS`, pinned to tag `v10.3.0`) rather than a
system install. After cloning the workspace:

```bash
git submodule update --init --recursive
cargo build -p horndb-closure
```

- **Requirements:** `cmake` + a C compiler, and — for the default
  `openmp` feature — an OpenMP runtime (`libomp` on macOS via
  `brew install libomp cmake`; `libgomp`, shipped with gcc, on Linux).
  **No** system GraphBLAS and **no** libclang are required for a normal
  build.
- **Cargo features:** `vendored` *(default)* compiles the submodule via
  the `cmake` crate into the shared `.shared-build/` dir (see *First build
  cost* below) and links it **statically**; `openmp`
  *(default)* builds GraphBLAS with OpenMP; `regen-bindings` *(off)*
  re-runs bindgen (the only path that needs libclang) — otherwise the
  checked-in `src/bindings.rs` is used. `--no-default-features` falls back
  to a `pkg-config` probe of a system GraphBLAS.
- **First build cost:** the vendored GraphBLAS compile takes ~1–3 min on a
  cold build; thereafter it is **shared across git worktrees**. `build.rs`
  compiles it once per `(target, version)` into
  `<main-worktree>/crates/closure/vendor/.shared-build/<target>/<version>/`
  (gitignored) and every worktree on that version reuses the artifact. A
  `.complete` sentinel marks a usable install; concurrent builders serialise on
  an advisory `flock` (via `fs4`) on `.build.lock`, into which the active
  builder writes its pid so a waiting build can report who holds the lock.
  Waiters poll for up to 30 minutes. The `flock` — not the pid — is the
  correctness mechanism: if a builder dies, the kernel releases the lock and the
  next waiter takes over. A worktree pinned to a *different* GraphBLAS version
  builds its own copy under a separate `<version>` dir. **Caveat:** `flock` over
  NFS is historically unreliable, so `.shared-build` must not sit on a network
  mount. If git is unavailable (e.g. a source tarball), the build falls back to
  a crate-local `vendor/.shared-build/<target>/<version>/` (no cross-worktree
  sharing). CI caches the shared dir (`.github/workflows/ci.yml`) keyed on the
  submodule SHA.
- **JIT:** built with `GRAPHBLAS_USE_JIT=OFF`. Standard semirings hit
  GraphBLAS's precompiled FactoryKernels, so no runtime C compiler is
  needed. If valued-closure custom semirings are ever required, PreJIT
  them into the vendored library rather than enabling runtime JIT.

## SPEC-08 integration

These notes describe call sites that **SPEC-05's plan** is responsible
for implementing.

## F1 cascade — `sameAs` equivalence-class merge

When SPEC-04 admits a candidate `owl:sameAs(a, b)` from the staging
graph, SPEC-05's `EQREL` structure must:

1. Compute the implied equivalence-class consequences (union of the
   two classes, transitive over all property assertions touching
   either class).
2. Tag every newly-derived triple with the originating
   `MlProvenance::MlDerived { model, confidence }` so the audit
   trail (F6) can attribute the cascade back to the candidate.
3. Per SPEC-08's "sameAs cascade" risk: this is expensive to roll
   back. Stage 1's "always queue for review" policy keeps the cascade
   in the staging graph until accepted; the commit step then bulk-
   inserts via the writeback path described in SPEC-05 F5.

No `horndb-closure` API needs to change for Stage 0/1 — this
integration is a SPEC-05 plan task that calls into `horndb-ml`'s
existing types only.

## Valued-reasoning readiness metrics (#11)

Instrumentation added *before* any custom-semiring acceleration (#12), so the
build/buy call for JIT/PreJIT is measured, not guessed.

- `grb::ValuedMatrix` — an `f64`-valued GraphBLAS matrix (the Fork-A scalar
  confidence carrier). Two `(max, ×)` multiply paths are exposed on purpose:
  `mxm_max_times_builtin` (prepackaged `GrB_MAX_TIMES_SEMIRING_FP64`,
  FactoryKernel) and `mxm_max_times_udf` (a `(max, ×)` semiring assembled from
  user-defined ops via `grb::UserSemiring`, forcing SuiteSparse's generic
  kernel). The throughput ratio between the two is exactly the multiplier a
  JIT/PreJIT specialization would remove.
- `grb::UserSemiring` owns its `GrB_BinaryOp`/`GrB_Monoid`/`GrB_Semiring`
  handles (RAII free on drop) so they outlive any `GrB_mxm` referencing them.
  The two `extern "C"` user ops (`udf_max`, `udf_times`) are plain scalar-FP64
  functions matching `GxB_binary_function`.
- `metrics::valued_transitive_closure` runs a best-confidence-path `(max, ×)`
  closure and returns `metrics::ClosureMetrics`: `N`, input/closure `nnz`,
  density, iterations-to-fixpoint, per-iteration frontier-`nnz` work profile,
  `GrB_mxm` time vs total time (`mxm_share`), and the `CarrierShape`. Selecting
  `ValuedKernel::{Builtin, Udf}` toggles the kernel under test.
- `benches/valued_readiness.rs` produces the two decision-relevant numbers
  (valued-vs-boolean kernel split; generic-kernel penalty). Results +
  decision rule live in `docs/benchmarks.md`. `tests/valued_closure.rs` pins
  correctness, including that the builtin and UDF kernels are bit-identical.

**Why FP64 / `(max, ×)` and not the boolean wrapper:** boolean matrices can use
SuiteSparse's iso/bitmap fast paths; a valued carrier cannot, which is the cost
this metric quantifies. The measured generic-kernel penalty on this v10.3.0
build for a *scalar* FP64 op is ~1.0×, so PreJIT is only worth revisiting for a
*structured* (UDT) carrier — recorded as the #12 gate.

## Incremental closure retraction (SPEC-24 S2, #211)

`IncrementalTransitiveClosure::delete_edge` is **output-sensitive by default**.
The default `DeleteStrategy::SupportCounting` is a two-phase decremental sweep:
remove the base edge, seed a worklist with only the closed pairs that could have
used the deleted first hop (over-delete candidates), then re-derive by checking
each pair's live *support* (does any base out-edge still witness the target?),
cascading backward along `bwd_base` only through pairs that lose all support.
Cost is proportional to the closure delta plus the inspected frontier, not the
store size. No stored counters — support is computed on the fly — so the insert
path is unchanged.

The original full-recompute algorithm is retained as
`DeleteStrategy::Recompute` (set per instance with `set_delete_strategy`). The
differential proptests run **both** strategies against the GraphBLAS bulk
closure, so Recompute stays in-tree as the oracle and can be flipped on per
predicate if a future density ever favours it.

`last_delete_probes()` returns the number of pairs the most recent `delete_edge`
inspected (seeded + support evaluations). It is the deterministic,
CI-stable gate for output-sensitivity — a wall-clock bench is env-sensitive,
this counter is not. `benches/closure_retraction.rs` is the A/B (support-counting
vs recompute); recorded numbers come from hornbench.

Seeding: `IncrementalClosureBackend::seed_base_edges` (and the SPEC-06 wrapper
`TransitiveClosureRule::seed_base_edges`) seed from the **true asserted base**
and re-close once, so retracting any seeded edge is **exact**. The older
`seed_transitive_closure` seeds an already-closed extent as a conservative
stand-in base; retracting a seeded edge there is sound but may under-withdraw
(a redundant transitive edge can keep a pair reachable). Use the base seed when
the store can supply the asserted base; use the closed-extent seed only when the
true base is unavailable.
