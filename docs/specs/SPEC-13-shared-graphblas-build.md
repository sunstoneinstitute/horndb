---
status: approved
date: 2026-05-31
scope: "Shared, lock-guarded GraphBLAS build across worktrees"
---

# Shared, lock-guarded GraphBLAS build across worktrees

**Date:** 2026-05-31
**Status:** Approved design — ready for implementation plan
**Crate:** `horndb-closure` (`crates/closure/build.rs`)

## Problem

`crates/closure/build.rs::build_vendored()` compiles SuiteSparse:GraphBLAS
(vendored submodule, currently `v10.3.0`) from source into the per-crate
`OUT_DIR`. `OUT_DIR` lives under the invoking target directory, which is
**per-worktree** by default. The multi-agent Stage-1 workflow runs several git
worktrees under `.claude/worktrees/`, so the same GraphBLAS gets recompiled once
per worktree — a 1–3 minute cold compile and a multi-hundred-MB build tree each
time. The existing workaround ("point `CARGO_TARGET_DIR` at a shared path")
shares *everything* (including the ~700 MB rocksdb artifact) and is coarser than
needed.

## Goal

Compile GraphBLAS **once per version**, into a shared location anchored to the
**main worktree**, and have every other worktree on that same GraphBLAS version
reuse the artifact. Serialize concurrent builders so only one compiles while the
rest wait. A worktree pinned to a *different* GraphBLAS version builds its own
copy independently, with no interference.

Non-goals: changing the linking model (still static, still via the existing
`pkg-config` probe), changing the `regen-bindings` path, or sharing any artifact
other than GraphBLAS.

## Design

### Layout

All paths derived inside `build_vendored()`:

- **Version** `<ver>` (e.g. `10.3.0`): parsed from the **current worktree's**
  `vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake` —
  the `GraphBLAS_VERSION_MAJOR`, `_MINOR`, `_SUB` `set(... CACHE STRING ...)`
  lines.
- **Target** `<target>` (e.g. `aarch64-apple-darwin`): the build-script `TARGET`
  env var. GraphBLAS compiles to architecture-specific machine code, so the
  artifact must not be shared across CPU architectures (Apple Silicon vs x86_64,
  or any cross-compile). The full target triple is used rather than the bare CPU
  arch because it also distinguishes OS/ABI, which matters if `.shared-build`
  ever sits on a mount visible to more than one platform.
- **Reuse key** = `<target>/<ver>` (version-only *within* a target, as agreed).
- **Main worktree root** `<main>`: parent of
  `git rev-parse --path-format=absolute --git-common-dir` (which yields
  `<main>/.git`); the shared dir is then
  `<main>/crates/closure/vendor/.shared-build/<target>/<ver>/`.
  If git is unavailable (e.g. building from a source tarball), fall back to a
  crate-local shared dir `CARGO_MANIFEST_DIR/vendor/.shared-build/<target>/<ver>/`
  so the build still works (it simply won't be shared across worktrees in that
  case).
- **Shared dir**: `<main>/crates/closure/vendor/.shared-build/<target>/<ver>/`
  - `install/` — the cmake install prefix (`lib/pkgconfig/GraphBLAS.pc`,
    headers, the static `.a`). This is the `out_dir` handed to `cmake::Config`.
  - `.build.lock` — advisory lock file; also holds the builder's pid as text.
  - `.complete` — sentinel file written **only** after a fully-successful
    cmake build+install. Its presence is the sole "this install is usable"
    signal (cmake install is not atomic; a partial `install/` tree is not
    enough).

**Why "current worktree's version, main worktree's location":** a worktree on a
newer GraphBLAS (say `10.4.0`) parses `10.4.0`, builds from *its own*
`vendor/GraphBLAS` into
`<main>/crates/closure/vendor/.shared-build/<target>/10.4.0/`, fully separate
from `10.3.0`. That is the "one worktree can build with a newer dependency
separately" requirement.

`crates/closure/vendor/.shared-build/` is added to the root `.gitignore`.

### Locking

`fs4` (new build-dependency) provides `try_lock_exclusive` — `flock(LOCK_EX |
LOCK_NB)` on unix, no `unsafe`, no hand-rolled libc. The lock file is opened
`O_CREAT | O_RDWR` (no `O_EXCL`: flock provides exclusion, not creation). Because
all worktrees reference the same `<main>/.../.build.lock` path, they share one
inode and the lock is global across processes regardless of which worktree
launched them.

`flock` is **auto-released when the holding process dies or its fd closes.**
This is the reason flock is the correctness mechanism and the pid is only a
diagnostic: if a builder crashes, the kernel drops the lock and the next waiter's
acquire simply succeeds — there is no stale-lock reclaim race to reason about.
The pid written into the lock file lets a waiting build print *"waiting for
GraphBLAS 10.3.0, held by pid N"* instead of a silent stall.

### Control flow (replaces the body of `build_vendored`)

```
ver     = parse_version(<this-worktree>/vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake)
target  = env("TARGET")                 # e.g. aarch64-apple-darwin
shared  = main_root()/crates/closure/vendor/.shared-build/<target>/<ver>
install = shared/install ;  marker = shared/.complete ;  lock = shared/.build.lock

if marker exists:                       # fast path — no cmake, no lock
    use(install); return

mkdir_p(shared)
fd       = open(lock, O_CREAT|O_RDWR)
deadline = now + 30min
loop:
    match try_lock_exclusive(fd):
        Ok (we hold it):
            if marker exists:           # someone completed during the acquire race
                unlock; use(install); return
            truncate(fd); write(fd, "<our_pid>\n")
            cmake_build(src=<this-worktree>/vendor/GraphBLAS, out_dir=install)
            write(marker)
            drop(fd)                     # flock released on close
            use(install); return
        WouldBlock:
            if marker exists:
                use(install); return
            pid = read_pid(lock)         # best-effort, diagnostic only
            log "waiting for GraphBLAS <ver> build, held by pid {pid}"
            if pid is Some and not alive(pid):
                log "builder pid {pid} appears gone; retrying"   # info, NOT reclaim
            if now > deadline:
                error: "GraphBLAS <ver> build still locked after 30 min
                        (lock: {lock}, holder pid: {pid}); remove the lock
                        file if the build is wedged"
            sleep(2s); continue
```

- `alive(pid)`: `kill -0 <pid>` via `std::process::Command` (works on macOS and
  Linux; no new dependency). Diagnostic only — correctness is flock's job.
- `use(install)`: set `PKG_CONFIG_PATH` to `install/lib/pkgconfig` and
  `install/lib64/pkgconfig` (prepended to any existing value), then let the
  existing `probe_graphblas()` resolve the library and emit link flags.

### Things that move, not just the build

These currently live *inside* the build branch and must run on the **reuse** path
too, because linking happens regardless of who compiled:

1. The macOS/OpenMP `cargo:rustc-link-search=native=<libomp>/lib` directive.
2. The `PKG_CONFIG_PATH` setup pointing at the install (now the shared install).

Also add `cargo:rerun-if-changed=vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake`
so a submodule version bump retriggers `build.rs`.

`probe_graphblas()`, the `regen-bindings` path, and the `--no-default-features`
(system GraphBLAS) path are unchanged.

### Testability

Factor the pure logic into a `crates/closure/build/shared.rs` helper that
`build.rs` pulls in with `include!`. Unit-testable, no cmake/git/IO required for
the core decisions:

- `parse_version(contents: &str) -> String` — given the cmake file text, returns
  `"10.3.0"`. Tests: nominal, whitespace variance, missing field.
- The wait-loop **decision** as a pure step function over injected inputs
  (`marker_exists`, `lock_acquired`, `pid_alive`, `now`, `deadline`) returning an
  enum (`Build | UseInstall | Wait | Fail`). Tests cover: fast path, win-the-lock,
  marker-appears-while-waiting, deadline-exceeded, dead-holder-keeps-waiting
  (flock will let us through on the next real acquire).

The cmake invocation, the real flock, and the git call stay untested (integration
concerns); the helper isolates everything that benefits from a unit test.

## Caveats (recorded, accepted)

- **flock over NFS is historically unreliable.** Irrelevant for local dev
  builds, but `.shared-build` must not be pointed at a network mount. One
  sentence to this effect goes in `INTEGRATION-NOTES.md`.
- Reuse key is **version only within a target** (`<target>/<ver>`): if someone
  moves the submodule to a *different commit that still reports the same version
  string*, the stale build is reused until `.shared-build/<target>/<ver>/` is
  cleared by hand. Accepted — release tags are immutable in practice.

## Docs to update in the same change

- `crates/closure/INTEGRATION-NOTES.md` — document the shared `.shared-build/<ver>/`
  layout, the flock+pid lock, the 30-min wait, and the NFS caveat.
- `CLAUDE.md` — the closure gotcha and the "point `CARGO_TARGET_DIR` at a shared
  path" note now apply only to **rocksdb** (harness), not GraphBLAS; clarify.
- `TASKS.md` — check for the LOW "disk pressure during parallel worktree runs"
  operational item; cross-reference or update it (and its mirrored GitHub issue
  per the repo's sync rule) if present.
