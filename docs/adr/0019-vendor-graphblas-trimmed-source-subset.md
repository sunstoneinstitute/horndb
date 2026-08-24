# ADR-0019: Vendor a trimmed SuiteSparse:GraphBLAS source subset, not a submodule

**Status:** Accepted

**Date:** 2026-08-24

**Source:** Supersedes [ADR-0015](0015-vendor-graphblas-static-submodule.md). Everything else that ADR decided — static linkage, checked-in bindings, the flock-guarded shared build — still holds.

## Context

ADR-0015 vendored GraphBLAS as a git submodule. That solved the system-dependency problem but left a per-clone and per-worktree cost: a 16 MB submodule fetch, and a `git submodule update --init` step that every worktree, CI job, and release tarball had to remember. `.claude/scripts/next-task.sh` carried a special case for it; three CI jobs set `submodules: recursive`; the release workflow spliced the submodule into the source tarball by hand. Forgetting any of them produced a confusing `build.rs` panic.

Most of the submodule is not used. `crates/closure/build.rs` runs a cmake build with `GRAPHBLAS_USE_JIT=OFF`, so the build reads only the sources, headers, cmake modules, and bundled third-party code. Upstream's documentation (7.5 MB), MATLAB interface (8.7 MB), test suite (3.9 MB), logo (2.5 MB), and demos were being fetched and checked out for nothing.

## Decision

Check a trimmed subset of the upstream source tree into the repository, and re-derive it with a script instead of maintaining it by hand.

- Vendor ~43 MB / 3875 files (of 66 MB / 5272) under `crates/closure/vendor/GraphBLAS`, as ordinary tracked files. Dropped: `Doc/`, `GraphBLAS/` (MATLAB), `Test/`, `Tcov/`, `logo/`, `Demo/` except `Include/`. `CUDA/` (500 KB) is kept although upstream forces it off, so a future GPU configuration needs no re-vendor.
- Keep the tree a **byte-exact subset of an upstream tag** — never patched. Re-running the script at the recorded tag must leave `git diff` empty; that is the drift check, and it replaces a patch stack.
- `crates/closure/vendor/refresh-graphblas.sh <tag>` clones upstream, copies the paths in `graphblas-keep.txt`, writes provenance to `GraphBLAS.vendor.md`, and verifies by running `cargo nextest run -p horndb-closure` — the real `build.rs`, so cmake flags are not duplicated.
- `graphblas-keep.txt` is an **allowlist**. A future version that needs an unlisted directory fails the script's verify build during the upgrade, rather than someone else's build later.
- Key the CI cache on `git rev-parse HEAD:crates/closure/vendor/GraphBLAS` — the directory's tree hash — which changes on both a version bump and a keep-list change.

## Consequences

+ A plain clone builds. No submodule step in worktrees, CI, or the release tarball; the special cases in `next-task.sh`, `ci.yml`, and `release-artifacts.yml` are gone.
+ Clone cost drops: the subset is ~3.7 MB compressed against the submodule's 16 MB pack.
+ Upgrading is one command plus a commit, and the upgrade is reviewable as a diff.
− Each version bump rewrites ~3875 files, roughly 3.7 MB compressed, and that history is permanent. A submodule would have shipped a delta.
− The vendored sources appear in workspace-wide greps and file counts. `.gitattributes` marks them `linguist-vendored -diff` to limit the noise.
− Trimming is a judgement call that upstream does not know about; an upstream reorganisation surfaces as a failed upgrade rather than a silent one. This is the intended trade.

## Related

- Supersedes: ADR-0015.
- Governing spec: `docs/specs/SPEC-05-closure-backend.md`.
- Design: `docs/specs/SPEC-13-shared-graphblas-build.md` (the shared build is unchanged).
- Architecture: `docs/architecture.md` §7.
- Integration notes: `crates/closure/INTEGRATION-NOTES.md`.
