# `horndb-closure` (SPEC-05) — agent notes

GraphBLAS closure backend. Links to SuiteSparse:GraphBLAS via `build.rs` +
`bindgen` + `pkg-config` (`links = "graphblas"`).

- `build.rs` bindgens against `wrapper.h` and `pkg-config`s `graphblas`; you need
  SuiteSparse:GraphBLAS installed locally to build this crate. Wrapper headers live
  alongside `Cargo.toml`.
- The vendored build is compiled once per `(target, version)` into a flock-guarded
  `vendor/.shared-build/<target>/<version>/`, shared across worktrees.

See `INTEGRATION-NOTES.md` for the full integration story and design decisions.
