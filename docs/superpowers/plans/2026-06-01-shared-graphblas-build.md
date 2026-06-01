# Shared, lock-guarded GraphBLAS build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile the vendored SuiteSparse:GraphBLAS once per `(target, version)` into a main-worktree-anchored `.shared-build/<target>/<ver>/` dir, reused across git worktrees and CI, serialised by an advisory flock with the builder pid written in for diagnostics.

**Architecture:** `crates/closure/build.rs::build_vendored()` stops compiling into the per-crate `OUT_DIR`. It derives the GraphBLAS version (from the submodule's cmake version file) and the build target (triple), computes a shared install dir under the **main** worktree, and either reuses a completed build (`.complete` marker present) or acquires an `fs4` flock and runs cmake into the shared dir. All pure decision logic (version parse, lock-step state machine, pid parse) lives in `crates/closure/build/shared.rs`, unit-tested via `tests/build_shared.rs`. CI gains an `actions/cache` step for the shared dir (it now lives outside `target/`, so `rust-cache` no longer covers it).

**Tech Stack:** Rust 1.88, `cmake` crate (0.1), `pkg-config` crate, `fs4` 1.1 (advisory flock; default `sync` feature), GitHub Actions (`actions/cache@v4`).

**Design spec:** `docs/superpowers/specs/2026-05-31-shared-graphblas-build-design.md`

---

## File Structure

- **Create** `crates/closure/build/shared.rs` — pure, dependency-free (std-only) helpers: `parse_version`, `parse_pid`, `decide` + `LockStep`. `#[path]`-included by both `build.rs` and the test file. No IO, no fs4, no cmake — everything here is unit-testable.
- **Create** `crates/closure/tests/build_shared.rs` — unit tests for the helpers, `#[path = "../build/shared.rs"] mod shared;`.
- **Modify** `crates/closure/build.rs` — rewrite `build_vendored()` to do version/target/path derivation, the lock loop, cmake-into-shared-install, and to emit `PKG_CONFIG_PATH` + the OpenMP link-search on **both** the build and reuse paths. `probe_graphblas()` and `generate_bindings()` are unchanged.
- **Modify** `crates/closure/Cargo.toml` — add `fs4` as an optional build-dependency gated on the `vendored` feature.
- **Modify** `.gitignore` (repo root) — ignore `crates/closure/vendor/.shared-build/`.
- **Modify** `.github/workflows/ci.yml` — add an `actions/cache@v4` step for the shared dir keyed on the GraphBLAS submodule SHA + OS/arch.
- **Modify** `crates/closure/INTEGRATION-NOTES.md`, `CLAUDE.md`, `TASKS.md` — documentation sync.

---

## Task 1: Pure helper — `parse_version`

**Files:**
- Create: `crates/closure/build/shared.rs`
- Test: `crates/closure/tests/build_shared.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/build_shared.rs`:

```rust
//! Unit tests for the pure helpers in `build/shared.rs`. The helper file is
//! `#[path]`-included here (it is also included by `build.rs`); it is std-only
//! so it compiles in the test target regardless of crate features.
#[path = "../build/shared.rs"]
mod shared;

const VERSION_CMAKE: &str = r#"
# version of SuiteSparse:GraphBLAS
set ( GraphBLAS_DATE "Dec 3, 2025" )
set ( GraphBLAS_VERSION_MAJOR 10 CACHE STRING "" FORCE )
set ( GraphBLAS_VERSION_MINOR 3 CACHE STRING "" FORCE )
set ( GraphBLAS_VERSION_SUB   0 CACHE STRING "" FORCE )

set ( GraphBLAS_API_VERSION_MAJOR 2 )
message ( STATUS "Building SuiteSparse:GraphBLAS version: v"
    ${GraphBLAS_VERSION_MAJOR}.${GraphBLAS_VERSION_MINOR}.${GraphBLAS_VERSION_SUB} )
"#;

#[test]
fn parse_version_nominal() {
    assert_eq!(shared::parse_version(VERSION_CMAKE).as_deref(), Some("10.3.0"));
}

#[test]
fn parse_version_ignores_api_version_and_message_lines() {
    // The file also mentions GraphBLAS_API_VERSION_MAJOR and ${...} message
    // interpolation; neither must be mistaken for the real version fields.
    assert_eq!(shared::parse_version(VERSION_CMAKE).as_deref(), Some("10.3.0"));
}

#[test]
fn parse_version_missing_field_returns_none() {
    let no_sub = "set ( GraphBLAS_VERSION_MAJOR 10 CACHE STRING \"\" FORCE )\n\
                  set ( GraphBLAS_VERSION_MINOR 3 CACHE STRING \"\" FORCE )\n";
    assert_eq!(shared::parse_version(no_sub), None);
}
```

- [ ] **Step 2: Run test to verify it fails (compile error: module/file missing)**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: FAIL — `build/shared.rs` does not exist (`couldn't read .../build/shared.rs`).

- [ ] **Step 3: Create the helper with `parse_version`**

Create `crates/closure/build/shared.rs`:

```rust
//! Pure helpers for `build.rs`, isolated here so they can be unit-tested
//! (`tests/build_shared.rs`) without cmake, git, flock, or any IO. Keep this
//! file dependency-free: std only.

/// Parse the GraphBLAS version (e.g. `"10.3.0"`) from the contents of
/// `vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake`.
///
/// Looks for the three `set ( GraphBLAS_VERSION_{MAJOR,MINOR,SUB} <n> ... )`
/// lines and reads the first integer token after each field name. Lines that
/// reference the field via `${...}` interpolation (e.g. the `message(...)`
/// line) yield a non-numeric next token and are skipped.
pub fn parse_version(cmake: &str) -> Option<String> {
    let major = extract_field(cmake, "GraphBLAS_VERSION_MAJOR")?;
    let minor = extract_field(cmake, "GraphBLAS_VERSION_MINOR")?;
    let sub = extract_field(cmake, "GraphBLAS_VERSION_SUB")?;
    Some(format!("{major}.{minor}.{sub}"))
}

fn extract_field(cmake: &str, name: &str) -> Option<u64> {
    cmake.lines().find_map(|line| {
        let idx = line.find(name)?;
        let rest = &line[idx + name.len()..];
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/closure/build/shared.rs crates/closure/tests/build_shared.rs
git commit -F - <<'EOF'
feat(closure): add parse_version helper for shared GraphBLAS build

Pure, std-only helper that reads the GraphBLAS version from the submodule's
cmake version file. Unit-tested in tests/build_shared.rs.
EOF
```

---

## Task 2: Pure helper — `decide` + `LockStep` state machine

**Files:**
- Modify: `crates/closure/build/shared.rs`
- Test: `crates/closure/tests/build_shared.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/closure/tests/build_shared.rs`:

```rust
use shared::LockStep;

#[test]
fn decide_holding_lock_no_marker_builds() {
    assert_eq!(shared::decide(false, true, false), LockStep::Build);
}

#[test]
fn decide_holding_lock_marker_present_reuses() {
    // Won the lock, but the build completed during the acquire race.
    assert_eq!(shared::decide(true, true, false), LockStep::UseInstall);
}

#[test]
fn decide_lock_held_by_other_marker_present_reuses() {
    assert_eq!(shared::decide(true, false, false), LockStep::UseInstall);
}

#[test]
fn decide_lock_held_by_other_no_marker_waits() {
    assert_eq!(shared::decide(false, false, false), LockStep::Wait);
}

#[test]
fn decide_timed_out_fails() {
    assert_eq!(shared::decide(false, false, true), LockStep::Fail);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: FAIL — `LockStep` and `decide` undefined.

- [ ] **Step 3: Implement `decide` + `LockStep`**

Append to `crates/closure/build/shared.rs`:

```rust
/// One iteration's decision in the build-lock loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStep {
    /// We hold the lock and there is no completed build — compile now.
    Build,
    /// A completed build exists — use it (release the lock if we hold one).
    UseInstall,
    /// Someone else is building and the deadline has not passed — sleep & retry.
    Wait,
    /// Deadline exceeded while another process still holds the lock — give up.
    Fail,
}

/// Decide what to do this iteration.
///
/// Correctness rests on the flock, not on liveness checks: if the builder dies,
/// the kernel releases its flock and a later iteration acquires it. So holder
/// liveness is *not* an input here — it is logged by the caller for diagnostics
/// only. `timed_out` is consulted solely when we are still waiting.
pub fn decide(marker_exists: bool, lock_acquired: bool, timed_out: bool) -> LockStep {
    if lock_acquired {
        if marker_exists {
            LockStep::UseInstall
        } else {
            LockStep::Build
        }
    } else if marker_exists {
        LockStep::UseInstall
    } else if timed_out {
        LockStep::Fail
    } else {
        LockStep::Wait
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: PASS (8 tests total).

- [ ] **Step 5: Commit**

```bash
git add crates/closure/build/shared.rs crates/closure/tests/build_shared.rs
git commit -F - <<'EOF'
feat(closure): add lock-loop decision state machine

Pure decide()/LockStep covering fast-path reuse, win-the-lock, completed-
during-race, wait, and deadline cases. flock handles crash reclaim, so holder
liveness is diagnostic only and not an input to the decision.
EOF
```

---

## Task 3: Pure helper — `parse_pid`

**Files:**
- Modify: `crates/closure/build/shared.rs`
- Test: `crates/closure/tests/build_shared.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/closure/tests/build_shared.rs`:

```rust
#[test]
fn parse_pid_reads_first_line() {
    assert_eq!(shared::parse_pid("48213\n"), Some(48213));
}

#[test]
fn parse_pid_trims_whitespace() {
    assert_eq!(shared::parse_pid("  61\n\n"), Some(61));
}

#[test]
fn parse_pid_rejects_garbage_and_empty() {
    assert_eq!(shared::parse_pid(""), None);
    assert_eq!(shared::parse_pid("not-a-pid"), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: FAIL — `parse_pid` undefined.

- [ ] **Step 3: Implement `parse_pid`**

Append to `crates/closure/build/shared.rs`:

```rust
/// Parse a pid written by the build holder into the lock file. Best-effort,
/// used only for the "waiting for pid N" diagnostic.
pub fn parse_pid(contents: &str) -> Option<u32> {
    contents.trim().lines().next()?.trim().parse::<u32>().ok()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-closure --test build_shared`
Expected: PASS (11 tests total).

- [ ] **Step 5: Commit**

```bash
git add crates/closure/build/shared.rs crates/closure/tests/build_shared.rs
git commit -F - <<'EOF'
feat(closure): add parse_pid helper for lock-file diagnostics
EOF
```

---

## Task 4: Wire `build.rs` to the shared, lock-guarded build

**Files:**
- Modify: `crates/closure/Cargo.toml` (add `fs4` build-dep)
- Modify: `crates/closure/build.rs` (rewrite `build_vendored`, add helpers, include `shared`)

- [ ] **Step 1: Add the `fs4` build-dependency, gated on `vendored`**

In `crates/closure/Cargo.toml`, change the `vendored` feature and `[build-dependencies]`:

```toml
[features]
default = ["vendored", "openmp"]
# Build the vendored SuiteSparse:GraphBLAS submodule from source.
vendored = ["dep:cmake", "dep:fs4"]
# Build GraphBLAS with OpenMP support.
openmp = []
# Regenerate the FFI bindings via bindgen instead of using the checked-in src/bindings.rs.
regen-bindings = ["dep:bindgen"]

[build-dependencies]
bindgen = { version = "0.69", optional = true }
pkg-config = "0.3"
cmake = { version = "0.1", optional = true }
# Advisory file lock (flock on unix) used to serialise the shared GraphBLAS
# build across worktrees. Default features include the `sync` std-File impl.
fs4 = { version = "1.1", optional = true }
```

- [ ] **Step 2: Replace `build.rs` with the shared-build implementation**

Overwrite `crates/closure/build.rs` with the following. `probe_graphblas()` and `generate_bindings()` are copied verbatim from the current file; everything in `build_vendored()` and the new helpers below it is the change.

```rust
#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::env;
#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::path::PathBuf;
#[cfg(feature = "vendored")]
use std::path::Path;
#[cfg(feature = "vendored")]
use std::process::Command;

/// Directory of the vendored SuiteSparse:GraphBLAS submodule headers.
#[cfg(feature = "regen-bindings")]
const VENDOR_INCLUDE: &str = "vendor/GraphBLAS/Include";

/// Path to the submodule's cmake version file (relative to this crate).
#[cfg(feature = "vendored")]
const VERSION_CMAKE: &str = "vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake";

/// Pure, unit-tested helpers (see `tests/build_shared.rs`).
#[cfg(feature = "vendored")]
#[path = "build/shared.rs"]
mod shared;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    // 1. Build (or reuse) the vendored submodule. Sets PKG_CONFIG_PATH so the
    //    shared probe below finds the freshly built (or cached) lib.
    #[cfg(feature = "vendored")]
    build_vendored();

    // 2. Probe GraphBLAS (vendored install or system) and emit the link flags.
    let lib = probe_graphblas();

    // 3. Regenerate bindings only when explicitly asked.
    #[cfg(feature = "regen-bindings")]
    generate_bindings(&lib);

    let _ = &lib;
}

/// Compile the vendored GraphBLAS once per `(target, version)` into a shared
/// dir anchored to the main worktree, reusing it across worktrees. Concurrent
/// builders are serialised with an advisory flock (`fs4`); the rest wait up to
/// 30 minutes. The builder writes its pid into the lock file for diagnostics.
#[cfg(feature = "vendored")]
fn build_vendored() {
    use std::fs;
    use std::time::{Duration, Instant};
    // NOTE: fs4's try_lock/unlock are called via fully-qualified trait syntax
    // (`fs4::FileExt::try_lock(&f)`) below. On Rust 1.88 the bare method names
    // collide with the soon-to-be-stabilised inherent std::fs::File methods,
    // which `-D warnings` rejects — so we do NOT `use fs4::FileExt;`.

    // A version bump (submodule move) must retrigger the build.
    println!("cargo:rerun-if-changed={VERSION_CMAKE}");

    let ver = read_graphblas_version();
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let shared = shared_build_dir(&target, &ver);
    let install = shared.join("install");
    let marker = shared.join(".complete");
    let lock_path = shared.join(".build.lock");

    if !marker.exists() {
        fs::create_dir_all(&shared)
            .unwrap_or_else(|e| panic!("creating {}: {e}", shared.display()));
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap_or_else(|e| panic!("opening {}: {e}", lock_path.display()));

        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        loop {
            let acquired = match fs4::FileExt::try_lock(&lock_file) {
                Ok(()) => true,
                Err(fs4::TryLockError::WouldBlock) => false,
                Err(e) => panic!("locking {}: {e}", lock_path.display()),
            };
            let timed_out = Instant::now() >= deadline;

            match shared::decide(marker.exists(), acquired, timed_out) {
                shared::LockStep::UseInstall => {
                    if acquired {
                        let _ = fs4::FileExt::unlock(&lock_file);
                    }
                    break;
                }
                shared::LockStep::Build => {
                    // Sticky note for any waiter: who is building.
                    let _ = fs::write(&lock_path, format!("{}\n", std::process::id()));
                    cmake_build_graphblas(&install);
                    fs::write(&marker, b"ok\n")
                        .unwrap_or_else(|e| panic!("writing {}: {e}", marker.display()));
                    let _ = fs4::FileExt::unlock(&lock_file);
                    break;
                }
                shared::LockStep::Wait => {
                    let holder = read_pid(&lock_path);
                    println!(
                        "cargo:warning=horndb-closure: waiting for GraphBLAS {ver} build \
                         (holder pid {holder:?}); this is normal across parallel worktrees"
                    );
                    if let Some(pid) = holder {
                        if !pid_is_alive(pid) {
                            println!(
                                "cargo:warning=horndb-closure: GraphBLAS builder pid {pid} \
                                 appears gone; will retry once its flock is released"
                            );
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
                shared::LockStep::Fail => panic!(
                    "GraphBLAS {ver} build still locked after 30 min (lock: {}, holder pid: {:?}); \
                     remove the lock file if the build is wedged",
                    lock_path.display(),
                    read_pid(&lock_path),
                ),
            }
        }
    }

    // Both the build and reuse paths must point the probe at the shared install
    // and emit the OpenMP link-search (linking happens regardless of who built).
    prepend_pkg_config_path(&install);
    emit_openmp_link_search();
}

/// Compute `<root>/crates/closure/vendor/.shared-build/<target>/<ver>` where
/// `<root>` is the main worktree. Falls back to a crate-local dir when git is
/// unavailable (e.g. building from a source tarball) — correctness is preserved,
/// only cross-worktree sharing is lost.
#[cfg(feature = "vendored")]
fn shared_build_dir(target: &str, ver: &str) -> PathBuf {
    let base = main_worktree_root()
        .map(|root| root.join("crates/closure/vendor/.shared-build"))
        .unwrap_or_else(|| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
                .join("vendor/.shared-build")
        });
    base.join(target).join(ver)
}

/// The main worktree root = parent of `git rev-parse --git-common-dir`
/// (which yields `<root>/.git`). `None` if git is unavailable or errors.
#[cfg(feature = "vendored")]
fn main_worktree_root() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    common.parent().map(Path::to_path_buf)
}

/// Read and parse the pinned GraphBLAS version from the submodule's cmake file.
#[cfg(feature = "vendored")]
fn read_graphblas_version() -> String {
    let contents = std::fs::read_to_string(VERSION_CMAKE)
        .unwrap_or_else(|e| panic!("reading {VERSION_CMAKE}: {e}"));
    shared::parse_version(&contents)
        .unwrap_or_else(|| panic!("could not parse GraphBLAS version from {VERSION_CMAKE}"))
}

/// Best-effort read of the pid the build holder wrote into the lock file.
#[cfg(feature = "vendored")]
fn read_pid(lock_path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(lock_path).ok()?;
    shared::parse_pid(&contents)
}

/// `kill -0 <pid>`: true if the process exists. Diagnostic only — flock, not
/// this check, is what makes a dead builder's lock reclaimable.
#[cfg(feature = "vendored")]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run the cmake build of `vendor/GraphBLAS`, installing into `install`.
#[cfg(feature = "vendored")]
fn cmake_build_graphblas(install: &Path) {
    let mut cfg = cmake::Config::new("vendor/GraphBLAS");
    cfg.out_dir(install)
        .profile("Release")
        .define("BUILD_TESTING", "OFF")
        // SuiteSparse defaults BUILD_SHARED_LIBS=ON. The GraphBLAS-specific
        // GRAPHBLAS_BUILD_STATIC_LIBS flag only *disables* the static build
        // when shared is on; it never forces static ON, so on its own it yields
        // only a `.dylib`, no `.a`, and an empty `Libs.private`. Turn shared off
        // and static on explicitly: this produces a real static archive AND
        // populates GraphBLAS.pc's Libs.private with the transitive deps (libm,
        // OpenMP) that `statik(true)` needs for a self-contained static link.
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_STATIC_LIBS", "ON")
        .define("GRAPHBLAS_USE_JIT", "OFF");

    #[cfg(feature = "openmp")]
    {
        let omp_prefix = openmp_prefix();
        cfg.define("OpenMP_ROOT", &omp_prefix);
        if cfg!(target_os = "macos") {
            // Help CMake's find_package(OpenMP) locate Homebrew libomp.
            cfg.define("CMAKE_PREFIX_PATH", &omp_prefix);
        }
    }

    #[cfg(not(feature = "openmp"))]
    {
        cfg.define("CMAKE_DISABLE_FIND_PACKAGE_OpenMP", "ON");
    }

    cfg.build();
}

/// Prepend the install's pkgconfig dir(s) to PKG_CONFIG_PATH so the probe
/// resolves the vendored library and its static `Libs.private` deps.
#[cfg(feature = "vendored")]
fn prepend_pkg_config_path(install: &Path) {
    let mut pc_dirs: Vec<PathBuf> = vec![
        install.join("lib").join("pkgconfig"),
        install.join("lib64").join("pkgconfig"),
    ];
    if let Some(existing) = env::var_os("PKG_CONFIG_PATH") {
        pc_dirs.extend(env::split_paths(&existing));
    }
    let joined = env::join_paths(pc_dirs).expect("joining PKG_CONFIG_PATH");
    // SAFETY: build scripts are single-threaded; set_var is fine here.
    unsafe {
        env::set_var("PKG_CONFIG_PATH", &joined);
    }
}

/// On macOS + OpenMP, the generated GraphBLAS.pc carries `-lomp` in
/// Libs.private but no `-L` for it (Homebrew libomp lives outside the default
/// search path), so the static link needs the directory. Emitted on BOTH the
/// build and reuse paths because linking happens regardless of who compiled.
#[cfg(feature = "vendored")]
fn emit_openmp_link_search() {
    #[cfg(feature = "openmp")]
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-search=native={}/lib", openmp_prefix());
    }
}

/// Discover the libomp prefix: LIBOMP_PREFIX, else HOMEBREW_PREFIX/opt/libomp,
/// else the Apple Silicon Homebrew default.
#[cfg(all(feature = "vendored", feature = "openmp"))]
fn openmp_prefix() -> String {
    env::var("LIBOMP_PREFIX")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            env::var("HOMEBREW_PREFIX")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|p| format!("{p}/opt/libomp"))
        })
        .unwrap_or_else(|| "/opt/homebrew/opt/libomp".to_string())
}

/// Probe GraphBLAS via pkg-config and emit `cargo:rustc-link-*` directives.
/// Static linkage is requested only in the vendored case.
fn probe_graphblas() -> pkg_config::Library {
    let mut cfg = pkg_config::Config::new();
    cfg.atleast_version("8.0");

    // The vendored static .pc carries OpenMP/libm in Libs.private; ask
    // pkg-config to resolve those by linking statically.
    #[cfg(feature = "vendored")]
    cfg.statik(true);

    match cfg.probe("GraphBLAS") {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!(
                "\n\
                =====================================================\n\
                horndb-closure: SuiteSparse:GraphBLAS not found.\n\
                pkg-config error: {e}\n\n\
                Either build the bundled copy (default features build the\n\
                vendored submodule) or install GraphBLAS and re-run with\n\
                --no-default-features:\n  \
                  macOS:         brew install suite-sparse pkg-config\n  \
                  Debian/Ubuntu: sudo apt-get install libsuitesparse-dev pkg-config\n\n\
                On Apple Silicon you may also need:\n  \
                  export PKG_CONFIG_PATH=/opt/homebrew/opt/suite-sparse/lib/pkgconfig:$PKG_CONFIG_PATH\n\
                =====================================================\n"
            );
            std::process::exit(1);
        }
    }
}

/// Regenerate the FFI bindings from `wrapper.h` against the GraphBLAS headers.
#[cfg(feature = "regen-bindings")]
fn generate_bindings(lib: &pkg_config::Library) {
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("GrB_.*")
        .allowlist_function("GxB_.*")
        .allowlist_type("GrB_.*")
        .allowlist_type("GxB_.*")
        .allowlist_var("GrB_.*")
        .allowlist_var("GxB_.*")
        .generate_comments(false)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &lib.include_paths {
        bindings_builder = bindings_builder.clang_arg(format!("-I{}", path.display()));
    }
    bindings_builder = bindings_builder.clang_arg(format!("-I{VENDOR_INCLUDE}"));

    let bindings = bindings_builder
        .generate()
        .expect("Unable to generate GraphBLAS bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
```

- [ ] **Step 3: Build the crate from a clean shared dir (verifies the build path)**

Run:
```bash
rm -rf crates/closure/vendor/.shared-build
cargo build -p horndb-closure 2>&1 | tail -20
```
Expected: GraphBLAS compiles (1–3 min), `crates/closure/vendor/.shared-build/<target>/10.3.0/install/lib*/pkgconfig/GraphBLAS.pc` and `.../.complete` exist, build succeeds.

Verify the artifact and marker:
```bash
find crates/closure/vendor/.shared-build -maxdepth 3 -name .complete -o -name 'GraphBLAS.pc' | sort
```
Expected: both the `.complete` marker and a `GraphBLAS.pc` are listed under `<target>/10.3.0/`.

- [ ] **Step 4: Rebuild to verify the reuse (fast) path**

Run:
```bash
touch crates/closure/build.rs   # force build.rs to re-run
cargo build -p horndb-closure 2>&1 | tail -20
```
Expected: completes in seconds, **no** GraphBLAS recompile (no cmake/cc output), build succeeds. The `.complete` fast path was taken.

- [ ] **Step 5: Run the closure unit + integration tests**

Run: `cargo test -p horndb-closure 2>&1 | tail -25`
Expected: all tests pass, including `build_shared` (11 tests) and the crate's existing tests.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/Cargo.toml crates/closure/build.rs Cargo.lock
git commit -F - <<'EOF'
feat(closure): build vendored GraphBLAS into a shared, flock-guarded dir

build_vendored() now compiles GraphBLAS once per (target, version) into
<main-worktree>/crates/closure/vendor/.shared-build/<target>/<ver>/, reused
across git worktrees. Concurrent builders serialise on an fs4 advisory flock
(crash-safe: the kernel releases it if the builder dies); the builder writes
its pid into the lock file so waiters can report who holds it. Waiters poll up
to 30 minutes. PKG_CONFIG_PATH and the macOS OpenMP link-search are now emitted
on both the build and reuse paths. Falls back to a crate-local dir when git is
unavailable.
EOF
```

---

## Task 5: Gitignore the shared build dir

**Files:**
- Modify: `.gitignore` (repo root)

- [ ] **Step 1: Add the ignore entry**

In `.gitignore`, add under the `/target` / `crates/*/target/` lines:

```gitignore
crates/closure/vendor/.shared-build/
```

- [ ] **Step 2: Verify it is ignored**

Run: `git status --porcelain crates/closure/vendor/.shared-build` and `git check-ignore crates/closure/vendor/.shared-build/x`
Expected: `git status` prints nothing for the dir; `check-ignore` echoes the path (it is ignored).

- [ ] **Step 3: Commit**

```bash
git add .gitignore
git commit -m 'build(closure): gitignore the shared GraphBLAS build dir'
```

---

## Task 6: Cache the shared GraphBLAS build in CI

**Files:**
- Modify: `.github/workflows/ci.yml`

**Why:** `rust-cache` (via `setup-rust-toolchain` `cache: true`) caches `target/`. The shared build now lives *outside* `target/`, so without an explicit cache CI would recompile GraphBLAS every run. This step keys the cache on the GraphBLAS submodule SHA so it invalidates exactly when the pin moves.

- [ ] **Step 1: Add the SHA-resolve + cache steps**

In `.github/workflows/ci.yml`, insert these two steps **after** the `setup-rust-toolchain` step and **before** the `rustfmt` step:

```yaml
      - name: resolve GraphBLAS submodule sha
        id: graphblas
        run: echo "sha=$(git -C crates/closure/vendor/GraphBLAS rev-parse HEAD)" >> "$GITHUB_OUTPUT"

      - name: cache vendored GraphBLAS build
        uses: actions/cache@v4
        with:
          path: crates/closure/vendor/.shared-build
          key: graphblas-${{ runner.os }}-${{ runner.arch }}-${{ steps.graphblas.outputs.sha }}
```

- [ ] **Step 2: Validate the workflow YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('ok')"`
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -F - <<'EOF'
ci: cache the shared vendored GraphBLAS build

The shared build now lives outside target/, so rust-cache no longer covers it.
Cache crates/closure/vendor/.shared-build keyed on the GraphBLAS submodule SHA
(+ OS/arch) so it invalidates exactly when the pin moves and CI stops
recompiling GraphBLAS on every run.
EOF
```

---

## Task 7: Documentation sync

**Files:**
- Modify: `crates/closure/INTEGRATION-NOTES.md`
- Modify: `CLAUDE.md`
- Modify: `TASKS.md`

- [ ] **Step 1: Update `crates/closure/INTEGRATION-NOTES.md`**

Replace the `**First build cost:**` bullet in the "Build: vendored SuiteSparse:GraphBLAS" section with:

```markdown
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
```

- [ ] **Step 2: Update `CLAUDE.md`**

In the "Build, test, lint" section, replace the sentence about `CARGO_TARGET_DIR`:

> If you run multiple worktrees in parallel, point `CARGO_TARGET_DIR` at a shared path so rocksdb is only compiled once across them.

with:

```markdown
If you run multiple worktrees in parallel, the vendored GraphBLAS is already
shared automatically (built once per `(target, version)` into
`crates/closure/vendor/.shared-build/`, flock-guarded — see
`crates/closure/INTEGRATION-NOTES.md`). The remaining large per-worktree
artifact is rocksdb (pulled in transitively by `horndb-harness`); point
`CARGO_TARGET_DIR` at a shared path if you want it compiled only once across
worktrees too.
```

In the "Crate-specific gotchas" → `horndb-closure` bullet, append:

```markdown
  The vendored build is compiled once per `(target, version)` into a
  flock-guarded `crates/closure/vendor/.shared-build/<target>/<version>/` shared
  across worktrees (details in `INTEGRATION-NOTES.md`).
```

- [ ] **Step 3: Update `TASKS.md` issue #13 cross-reference**

In the body entry for "Disk pressure during multi-agent runs" (the `- [ ] **Disk pressure during multi-agent runs.** ([#13]…)` paragraph), append a sentence noting the GraphBLAS portion is resolved:

```markdown
  *Update (2026-06-01):* the vendored GraphBLAS is no longer duplicated per
  worktree — `build.rs` compiles it once per `(target, version)` into a shared,
  flock-guarded `crates/closure/vendor/.shared-build/` dir (see
  `crates/closure/INTEGRATION-NOTES.md`). The remaining disk-pressure driver is
  `oxrocksdb-sys` under `horndb-harness`; `CARGO_TARGET_DIR` sharing is still the
  mitigation for that. Issue stays open until rocksdb duplication is addressed.
```

This is a scope-narrowing note, not a completion, so the `[ ]` marker and the architecture.md row stay as-is; issue #13 remains open. (No `gh issue close` — per the repo sync rule, only `[ ]`→`[x]` closes an issue.)

- [ ] **Step 4: Sync the GitHub issue #13 with a comment**

Run:
```bash
gh issue comment 13 --repo sunstoneinstitute/horndb --body "Vendored GraphBLAS is now built once per (target, version) into a shared, flock-guarded crates/closure/vendor/.shared-build/ dir, deduplicated across worktrees and cached in CI. Remaining disk-pressure driver is oxrocksdb-sys under horndb-harness; keeping this open for the rocksdb side."
```
Expected: comment posted (the issue stays open).

- [ ] **Step 5: Commit**

```bash
git add crates/closure/INTEGRATION-NOTES.md CLAUDE.md TASKS.md
git commit -F - <<'EOF'
docs: document shared flock-guarded GraphBLAS build

INTEGRATION-NOTES.md describes the .shared-build layout, the flock+pid lock, the
30-min wait, the NFS caveat, and the tarball fallback. CLAUDE.md clarifies the
CARGO_TARGET_DIR note now applies only to rocksdb. TASKS.md #13 narrowed to the
rocksdb side (stays open).
EOF
```

---

## Task 8: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Simulate a second worktree reusing the shared artifact**

Create a throwaway worktree and build closure in it; it must reuse the main worktree's `.shared-build` with **no** GraphBLAS recompile:

```bash
git worktree add /tmp/horndb-reuse-check HEAD
( cd /tmp/horndb-reuse-check && CARGO_TARGET_DIR=/tmp/horndb-reuse-target cargo build -p horndb-closure 2>&1 | tail -15 )
```
Expected: the second build does **not** recompile GraphBLAS (no cmake/cc output) and links against `<main>/crates/closure/vendor/.shared-build/<target>/10.3.0/install`. Confirm the shared dir was not duplicated into the new worktree:
```bash
ls /tmp/horndb-reuse-check/crates/closure/vendor/.shared-build 2>&1 || echo "no local .shared-build in the second worktree (expected)"
```
Expected: no `.shared-build` under the second worktree (it used the main one).

- [ ] **Step 2: Clean up the throwaway worktree**

```bash
git worktree remove /tmp/horndb-reuse-check --force
rm -rf /tmp/horndb-reuse-target
```

- [ ] **Step 3: Workspace clippy + build (what pre-push and CI run)**

Run:
```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15
```
Expected: no warnings, no errors. (First run may be slow due to rocksdb; that is unrelated to this change.)

- [ ] **Step 4: Run the closure benches' compile check (they link GraphBLAS)**

Run: `cargo build -p horndb-closure --benches 2>&1 | tail -10`
Expected: succeeds, reusing the shared install.

- [ ] **Step 5: Final confirmation**

Confirm the full task list is checked off and the working tree is clean except for intended changes:
```bash
git status
git log --oneline -8
```
Expected: 7 feature/doc commits from Tasks 1–7, clean tree.

---

## Self-Review Notes

- **Spec coverage:** layout (`<target>/<ver>`, main-worktree anchor, fallback) → Task 4 `shared_build_dir`/`main_worktree_root`; flock+pid lock + 30-min wait → Task 4 loop + Tasks 2/3 helpers; version parse → Task 1; `.complete` marker atomicity → Task 4; moved PKG_CONFIG_PATH + OpenMP link-search → Task 4 `prepend_pkg_config_path`/`emit_openmp_link_search`; `rerun-if-changed` on the version file → Task 4; gitignore → Task 5; CI cache (newly required) → Task 6; INTEGRATION-NOTES/CLAUDE.md/TASKS.md + NFS caveat + tarball fallback docs → Task 7; testability split → Tasks 1–3.
- **Type consistency:** `LockStep` variants (`Build`/`UseInstall`/`Wait`/`Fail`), `decide(marker_exists, lock_acquired, timed_out)`, `parse_version(&str) -> Option<String>`, `parse_pid(&str) -> Option<u32>` are used identically in tests (Tasks 1–3) and in `build.rs` (Task 4). `fs4::FileExt::try_lock` / `unlock` and `fs4::TryLockError::WouldBlock` match fs4 1.1.0.
- **No placeholders:** every code/command step is concrete.
