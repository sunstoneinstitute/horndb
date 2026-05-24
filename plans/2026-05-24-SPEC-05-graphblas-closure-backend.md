# SPEC-05 GraphBLAS Closure Backend — Stage-0/1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `reasoner-closure` crate with a working SuiteSparse:GraphBLAS FFI layer, schema-matrix construction for transitive properties / `rdfs:subClassOf` / `rdfs:subPropertyOf`, Boolean `(∨,∧)` semiring closure via iterated `GrB_mxm`, a union-find based `owl:sameAs` structure, dense-per-predicate ID renumbering, and a writeback path into the SPEC-02 store through a `TripleSink` trait. SPEC-04 will call us via a `ClosureBackend` trait we define here.

**Architecture:** A single workspace crate (`reasoner-closure`) split into focused modules. The crate links against a system-installed SuiteSparse:GraphBLAS via direct `bindgen`-generated FFI (no third-party GraphBLAS wrapper crate — the only existing ones are CC BY-NC 4.0 and incompatible with our Apache-2.0 license). A thin safe wrapper (`grb`) over the FFI exposes only the GraphBLAS surface we need: `GrB_Matrix`, `GrB_mxm`, monoids/semirings, `GrB_*_build`, `GrB_*_extractTuples`, `GrB_Matrix_nvals`. A `dense_id` module maintains a per-predicate `dictionary_id ↔ u64 dense index` map (FxHashMap + Vec). A `closure::transitive` module runs the iterated Boolean MxM until `nvals` stabilises. A `sameas` module implements union-find over dictionary IDs (pure Rust, no GraphBLAS) with canonical-representative = lexicographically smallest URI ID. A `sink` module defines the `TripleSink` trait that SPEC-02 will implement and exports a `ClosureBackend` trait that SPEC-04 consumes. The `build.rs` uses `pkg-config` to locate SuiteSparse:GraphBLAS; if missing, it prints a Homebrew/apt install hint and fails the build cleanly.

**Tech Stack:** Rust 2021 (workspace `rust-version = "1.75"`), `bindgen` 0.69+ (build-dep), `pkg-config` 0.3 (build-dep), `rustc-hash` (FxHashMap), `thiserror`, `anyhow`. External system dep: **SuiteSparse:GraphBLAS ≥ 8.x** (Homebrew: `brew install suite-sparse`; Debian/Ubuntu: `libsuitesparse-dev` or build from source for newer versions). No GPU, no LAGraph, no incremental update — those are deferred to Stage 2 / SPEC-09.

---

## External dependency the user must install BEFORE Stage 0

```bash
# macOS
brew install suite-sparse pkg-config

# Debian / Ubuntu
sudo apt-get install libsuitesparse-dev pkg-config
# (If the distro package is older than 8.x, build from source per SuiteSparse README.)
```

Verify with:

```bash
pkg-config --modversion GraphBLAS
# Expect: 8.x or 9.x or 10.x
```

If `pkg-config` does not know about `GraphBLAS`, set `PKG_CONFIG_PATH` to point at the directory containing `GraphBLAS.pc`. On Homebrew this is usually `/opt/homebrew/lib/pkgconfig` (Apple Silicon) or `/usr/local/lib/pkgconfig` (Intel mac).

---

## File Structure

Create the following under `crates/closure/`:

| Path | Responsibility |
|------|----------------|
| `crates/closure/Cargo.toml` | Crate manifest, `links = "graphblas"`, build-deps for bindgen + pkg-config |
| `crates/closure/build.rs` | Locate SuiteSparse:GraphBLAS via pkg-config, generate FFI bindings, emit link directives, friendly error on missing system lib |
| `crates/closure/wrapper.h` | One-line `#include <GraphBLAS.h>` header that bindgen consumes |
| `crates/closure/src/lib.rs` | Crate root: module declarations, re-exports, init/finalize lifecycle |
| `crates/closure/src/ffi.rs` | `include!(concat!(env!("OUT_DIR"), "/bindings.rs"));` — raw FFI |
| `crates/closure/src/error.rs` | `GrbError` enum (thiserror) over `GrB_Info` return codes |
| `crates/closure/src/grb.rs` | Safe RAII wrappers: `Matrix<bool>`, `init()` / `finalize()`, semiring constants, `mxm`, `build`, `extract_tuples`, `nvals`, `dup`, `equal` |
| `crates/closure/src/dense_id.rs` | `DenseIdMap` — bijection between `DictId(u64)` and `DenseIdx(u64)` per predicate |
| `crates/closure/src/closure/mod.rs` | Closure module root |
| `crates/closure/src/closure/transitive.rs` | `transitive_closure(M) -> M*` via iterated Boolean MxM |
| `crates/closure/src/closure/schema.rs` | `subclass_closure` / `subproperty_closure` (reuse transitive_closure; reflexivity per OWL 2 RL) |
| `crates/closure/src/sameas.rs` | `EquivClasses` — union-find over `DictId`; canonical rep = min URI ID |
| `crates/closure/src/sink.rs` | `TripleSink` trait (storage-side impl), `ClosureBackend` trait (rule-engine-facing API), `BackendImpl` (default implementation we provide) |
| `crates/closure/src/types.rs` | Newtype aliases: `DictId(u64)`, `PredicateId(u64)`, `Triple { s, p, o }`, `Edge { s, o }` |
| `crates/closure/tests/grb_smoke.rs` | Integration test: init/finalize GraphBLAS, build 2x2 matrix, mxm, check nvals |
| `crates/closure/tests/transitive_closure.rs` | Integration test: 5-node chain, square chain, cycle |
| `crates/closure/tests/sameas.rs` | Integration test: union-find correctness and canonical representative |
| `crates/closure/tests/end_to_end.rs` | Integration: build edges → closure → writeback through a `Vec<Triple>` `TripleSink` |
| `crates/closure/benches/transitive.rs` | Criterion bench: 2,500-node chain closure (gate for acceptance criterion 1) |
| `crates/closure/benches/sameas.rs` | Criterion bench: 10M sameAs across 1M canonicals (gate for acceptance criterion 3) |

---

## Task 0: Verify the external dependency is installed

**Files:** none.

- [ ] **Step 1: Verify SuiteSparse:GraphBLAS is reachable via pkg-config**

Run:

```bash
pkg-config --modversion GraphBLAS && pkg-config --cflags --libs GraphBLAS
```

Expected output: a version number (e.g. `8.3.1` or `10.1.1`) on stdout followed by `-I/...` and `-L/... -lgraphblas` flags.

If the command fails with `Package GraphBLAS was not found in the pkg-config search path`:
- On macOS run `brew install suite-sparse pkg-config` and re-run.
- On Apple Silicon also: `export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH`.
- On Debian/Ubuntu run `sudo apt-get install libsuitesparse-dev pkg-config`.

Do **not** proceed to Task 1 until this step succeeds. The plan assumes a working system install.

---

## Task 1: Bootstrap the `reasoner-closure` crate manifest

**Files:**
- Modify: `crates/closure/Cargo.toml`

- [ ] **Step 1: Replace the empty manifest with a real one**

Write the file:

```toml
[package]
name = "reasoner-closure"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false
links = "graphblas"

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
rustc-hash = "2"

[build-dependencies]
bindgen = "0.69"
pkg-config = "0.3"

[dev-dependencies]
anyhow = { workspace = true }

[[bench]]
name = "transitive"
harness = false

[[bench]]
name = "sameas"
harness = false
```

Note: `criterion` is intentionally **not** added here yet — benches are added in Task 13 once we have something to measure. The `[[bench]]` entries are pre-declared so `cargo` does not warn when the files appear.

Workaround: comment out the `[[bench]]` blocks for now until Task 13. Update the file to:

```toml
[package]
name = "reasoner-closure"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false
links = "graphblas"

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
rustc-hash = "2"

[build-dependencies]
bindgen = "0.69"
pkg-config = "0.3"

[dev-dependencies]
anyhow = { workspace = true }
```

- [ ] **Step 2: Confirm the workspace still resolves**

Run from repo root:

```bash
cargo check -p reasoner-closure 2>&1 | tail -20
```

Expected: `error[E0463]: can't find crate for ...` or a build-script error from `build.rs` — but NOT a manifest parse error and NOT an unresolved-dependency error from `bindgen` / `pkg-config` / `rustc-hash`. Those should resolve from crates.io.

If `bindgen` fails to resolve, run `cargo update` and retry.

- [ ] **Step 3: Commit**

```bash
git add crates/closure/Cargo.toml
git commit -m "$(cat <<'EOF'
closure: replace placeholder manifest with real dependencies

Sets up reasoner-closure as a linked crate against system
SuiteSparse:GraphBLAS, with bindgen + pkg-config build deps.
EOF
)"
```

---

## Task 2: Write a `build.rs` that finds SuiteSparse:GraphBLAS and generates bindings

**Files:**
- Create: `crates/closure/build.rs`
- Create: `crates/closure/wrapper.h`

- [ ] **Step 1: Create `wrapper.h`**

Write `crates/closure/wrapper.h`:

```c
#include <GraphBLAS.h>
```

That single line is all bindgen needs to discover the entire `GrB_*` / `GxB_*` surface.

- [ ] **Step 2: Create `build.rs`**

Write `crates/closure/build.rs`:

```rust
use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    let lib = match pkg_config::Config::new()
        .atleast_version("8.0")
        .probe("GraphBLAS")
    {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("\n\
                =====================================================\n\
                reasoner-closure: SuiteSparse:GraphBLAS not found.\n\
                pkg-config error: {e}\n\n\
                Install instructions:\n  \
                  macOS:       brew install suite-sparse pkg-config\n  \
                  Debian/Ubuntu: sudo apt-get install libsuitesparse-dev pkg-config\n\n\
                On Apple Silicon you may also need:\n  \
                  export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH\n\
                =====================================================\n");
            std::process::exit(1);
        }
    };

    // pkg_config already emits `cargo:rustc-link-lib=...` and `cargo:rustc-link-search=...`.
    // We still tell cargo about the include paths for bindgen.
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        // Allowlist GraphBLAS surface so we do not pull half of libc.
        .allowlist_function("GrB_.*")
        .allowlist_function("GxB_.*")
        .allowlist_type("GrB_.*")
        .allowlist_type("GxB_.*")
        .allowlist_var("GrB_.*")
        .allowlist_var("GxB_.*")
        // GraphBLAS uses a lot of macro-like extern constants we want as raw items.
        .generate_comments(false)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &lib.include_paths {
        bindings_builder = bindings_builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = bindings_builder
        .generate()
        .expect("Unable to generate GraphBLAS bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
```

- [ ] **Step 3: Run cargo build to verify the bindings generate**

```bash
cargo build -p reasoner-closure 2>&1 | tail -30
```

Expected: a successful build with no errors. `OUT_DIR/bindings.rs` should be ~hundreds of KB.

If you see `fatal error: 'GraphBLAS.h' file not found`, your `pkg-config` returned include paths bindgen's bundled clang cannot find. Workaround: `export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(brew --prefix)/include"` and rerun.

- [ ] **Step 4: Inspect a generated symbol to confirm bindings worked**

```bash
grep -c "pub fn GrB_mxm" target/debug/build/reasoner-closure-*/out/bindings.rs
```

Expected: `1` (the function declaration appears exactly once).

- [ ] **Step 5: Commit**

```bash
git add crates/closure/build.rs crates/closure/wrapper.h
git commit -m "$(cat <<'EOF'
closure: add build.rs that bindgens SuiteSparse:GraphBLAS

Uses pkg-config to locate the system library, generates allowlisted
FFI bindings via bindgen, and prints a Homebrew/apt install hint
when the system library is missing.
EOF
)"
```

---

## Task 3: Expose the raw FFI module and a typed error enum

**Files:**
- Modify: `crates/closure/src/lib.rs`
- Create: `crates/closure/src/ffi.rs`
- Create: `crates/closure/src/error.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/grb_smoke.rs`:

```rust
//! Smoke test: prove FFI surface compiles and we can convert error codes.

use reasoner_closure::error::GrbError;

#[test]
fn grb_success_is_ok() {
    // GrB_SUCCESS == 0; converting it to a GrbError result should be Ok(()).
    assert!(GrbError::check(0).is_ok());
}

#[test]
fn grb_nonzero_is_err() {
    // Any non-zero code is an error. Pick GrB_NULL_POINTER (typically -3 in v8.x, -103 in v10).
    let err = GrbError::check(-3);
    assert!(err.is_err());
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test grb_smoke 2>&1 | tail -10
```

Expected: `error[E0432]: unresolved import` — module `error` does not exist yet.

- [ ] **Step 3: Implement `ffi.rs`**

Create `crates/closure/src/ffi.rs`:

```rust
//! Raw FFI bindings to SuiteSparse:GraphBLAS, generated at build time by bindgen.
//!
//! Do not call these directly outside the `grb` safe-wrapper module.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
```

- [ ] **Step 4: Implement `error.rs`**

Create `crates/closure/src/error.rs`:

```rust
//! Typed error wrapping `GrB_Info` return codes.

use thiserror::Error;

/// Errors returned by the GraphBLAS C API. Wraps `GrB_Info`.
///
/// We do not enumerate every code — we keep the raw value for diagnostics
/// and only specialize the few we expect to handle programmatically.
#[derive(Debug, Error)]
pub enum GrbError {
    #[error("GraphBLAS returned non-success code {code}")]
    Failed { code: i32 },
}

impl GrbError {
    /// Convert a `GrB_Info` return value into `Result<(), GrbError>`.
    ///
    /// `0` is `GrB_SUCCESS`. Any other value is treated as an error.
    #[inline]
    pub fn check(code: i32) -> Result<(), Self> {
        if code == 0 {
            Ok(())
        } else {
            Err(Self::Failed { code })
        }
    }
}
```

- [ ] **Step 5: Update `lib.rs`**

Replace `crates/closure/src/lib.rs` with:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.
//!
//! Provides:
//! - Transitive-property closure via iterated Boolean MxM on SuiteSparse:GraphBLAS.
//! - `rdfs:subClassOf` and `rdfs:subPropertyOf` closures (same machinery).
//! - `owl:sameAs` equivalence classes via pure-Rust union-find.
//! - Per-predicate dense renumbering of dictionary IDs.
//! - Writeback into a `TripleSink` (implemented by the storage crate).

pub mod error;
pub mod ffi;
```

- [ ] **Step 6: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test grb_smoke 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/closure/src/lib.rs crates/closure/src/ffi.rs crates/closure/src/error.rs crates/closure/tests/grb_smoke.rs
git commit -m "$(cat <<'EOF'
closure: expose raw FFI module and typed GrbError

Wraps GrB_Info return codes in a thiserror enum. Raw bindings are
gated behind a private ffi module; only the safe wrapper (next task)
should call them.
EOF
)"
```

---

## Task 4: Implement GraphBLAS init/finalize lifecycle and a smoke MxM

**Files:**
- Create: `crates/closure/src/grb.rs`
- Modify: `crates/closure/src/lib.rs`
- Modify: `crates/closure/tests/grb_smoke.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/closure/tests/grb_smoke.rs`:

```rust
use reasoner_closure::grb::{init_once, BoolMatrix};

#[test]
fn init_and_build_bool_matrix() {
    init_once().expect("GrB_init");

    // Build a 3x3 boolean matrix with edges (0,1) and (1,2).
    let mat = BoolMatrix::from_edges(3, &[(0, 1), (1, 2)]).expect("build matrix");
    assert_eq!(mat.nvals().unwrap(), 2);
    assert_eq!(mat.nrows(), 3);
    assert_eq!(mat.ncols(), 3);
}

#[test]
fn boolean_mxm_one_step() {
    init_once().expect("GrB_init");

    // A: edges (0,1),(1,2). A*A should produce edge (0,2).
    let a = BoolMatrix::from_edges(3, &[(0, 1), (1, 2)]).unwrap();
    let c = a.mxm_lor_land(&a).unwrap();
    let edges = c.extract_edges().unwrap();
    assert_eq!(edges, vec![(0u64, 2u64)]);
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test grb_smoke 2>&1 | tail -10
```

Expected: `unresolved import reasoner_closure::grb`.

- [ ] **Step 3: Implement `grb.rs`**

Create `crates/closure/src/grb.rs`:

```rust
//! Safe RAII wrapper over the small slice of SuiteSparse:GraphBLAS we use.
//!
//! Only Boolean matrices are exposed. The Boolean `(∨,∧)` semiring
//! (`GxB_LOR_LAND_BOOL` or `GrB_LOR_LAND_SEMIRING_BOOL` depending on
//! the SuiteSparse version) is the only semiring we expose in Stage 1.

use std::sync::Once;

use crate::error::GrbError;
use crate::ffi;

static GRB_INIT: Once = Once::new();
static mut GRB_INIT_RESULT: i32 = 0;

/// Initialise GraphBLAS exactly once per process. Idempotent across threads.
///
/// SuiteSparse:GraphBLAS requires `GrB_init` before any other call and
/// `GrB_finalize` only at process shutdown (we deliberately do not call
/// `GrB_finalize` — the OS reclaims memory on exit and calling it from
/// a `Drop` of a static is unsound).
pub fn init_once() -> Result<(), GrbError> {
    // Safety: GrB_init is thread-safe via Once; mode = GrB_NONBLOCKING (0).
    unsafe {
        GRB_INIT.call_once(|| {
            GRB_INIT_RESULT = ffi::GrB_init(ffi::GrB_Mode_GrB_NONBLOCKING);
        });
        GrbError::check(GRB_INIT_RESULT)
    }
}

/// Owned Boolean GraphBLAS matrix. Frees the underlying `GrB_Matrix` on drop.
pub struct BoolMatrix {
    inner: ffi::GrB_Matrix,
    nrows: u64,
    ncols: u64,
}

// Safety: GrB_Matrix handles are independent allocations; SuiteSparse documents
// that distinct matrix handles may be used concurrently from different threads
// in GrB_NONBLOCKING mode (it serialises internally).
unsafe impl Send for BoolMatrix {}

impl BoolMatrix {
    /// Construct an `n x n` Boolean matrix populated from `edges`.
    pub fn from_edges(n: u64, edges: &[(u64, u64)]) -> Result<Self, GrbError> {
        let mut handle: ffi::GrB_Matrix = std::ptr::null_mut();
        unsafe {
            GrbError::check(ffi::GrB_Matrix_new(
                &mut handle,
                ffi::GrB_BOOL,
                n,
                n,
            ))?;
        }

        if !edges.is_empty() {
            let rows: Vec<u64> = edges.iter().map(|(s, _)| *s).collect();
            let cols: Vec<u64> = edges.iter().map(|(_, o)| *o).collect();
            let vals: Vec<bool> = vec![true; edges.len()];
            unsafe {
                // GrB_Matrix_build_BOOL(C, I, J, X, nvals, dup)
                // dup = GrB_LOR (any associative bool combiner; LOR is canonical).
                GrbError::check(ffi::GrB_Matrix_build_BOOL(
                    handle,
                    rows.as_ptr(),
                    cols.as_ptr(),
                    vals.as_ptr(),
                    edges.len() as u64,
                    ffi::GrB_LOR,
                ))?;
            }
        }

        Ok(Self { inner: handle, nrows: n, ncols: n })
    }

    /// Construct a fresh empty `n x n` Boolean matrix.
    pub fn new(n: u64) -> Result<Self, GrbError> {
        Self::from_edges(n, &[])
    }

    pub fn nrows(&self) -> u64 { self.nrows }
    pub fn ncols(&self) -> u64 { self.ncols }

    pub fn nvals(&self) -> Result<u64, GrbError> {
        let mut n: u64 = 0;
        unsafe { GrbError::check(ffi::GrB_Matrix_nvals(&mut n, self.inner))?; }
        Ok(n)
    }

    /// `C = A * B` over the `(∨, ∧)` Boolean semiring, replacing C.
    ///
    /// Returns a freshly allocated matrix; does not modify self or other.
    pub fn mxm_lor_land(&self, other: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
        assert_eq!(self.ncols, other.nrows, "shape mismatch for MxM");
        let mut c = BoolMatrix::new(self.nrows)?;
        unsafe {
            // GrB_mxm(C, Mask=NULL, accum=NULL, semiring, A, B, descriptor=NULL)
            GrbError::check(ffi::GrB_mxm(
                c.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GxB_LOR_LAND_BOOL,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(c)
    }

    /// `self ∨= other` — element-wise Boolean OR with `self` accumulating.
    /// Used to fold powers of `M` into the closure accumulator.
    pub fn or_assign(&mut self, other: &BoolMatrix) -> Result<(), GrbError> {
        assert_eq!(self.nrows, other.nrows);
        assert_eq!(self.ncols, other.ncols);
        unsafe {
            // GrB_Matrix_eWiseAdd_Semiring(C, M, accum, semiring, A, B, desc)
            // For union of Boolean matrices the simplest form is the LOR monoid.
            GrbError::check(ffi::GrB_Matrix_eWiseAdd_Monoid(
                self.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GrB_LOR_MONOID_BOOL,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(())
    }

    /// Force any pending GraphBLAS computation on this matrix to complete.
    ///
    /// Required between an `or_assign` and a subsequent `nvals` read because
    /// `GrB_init` was called in `GrB_NONBLOCKING` mode (operations are lazy).
    pub fn wait(&self) -> Result<(), GrbError> {
        unsafe {
            GrbError::check(ffi::GrB_Matrix_wait(
                self.inner,
                ffi::GrB_WaitMode_GrB_MATERIALIZE,
            ))
        }
    }

    /// Extract all `true` entries as `(row, col)` pairs in row-major order.
    pub fn extract_edges(&self) -> Result<Vec<(u64, u64)>, GrbError> {
        let nvals = self.nvals()?;
        let mut rows = vec![0u64; nvals as usize];
        let mut cols = vec![0u64; nvals as usize];
        let mut vals = vec![false; nvals as usize];
        let mut n_out: u64 = nvals;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_extractTuples_BOOL(
                rows.as_mut_ptr(),
                cols.as_mut_ptr(),
                vals.as_mut_ptr(),
                &mut n_out,
                self.inner,
            ))?;
        }
        rows.truncate(n_out as usize);
        cols.truncate(n_out as usize);
        let mut out: Vec<(u64, u64)> = rows.into_iter().zip(cols).collect();
        out.sort_unstable();
        Ok(out)
    }
}

impl Drop for BoolMatrix {
    fn drop(&mut self) {
        unsafe {
            // GrB_Matrix_free takes &mut handle; the free is best-effort on drop.
            let _ = ffi::GrB_Matrix_free(&mut self.inner);
        }
    }
}
```

- [ ] **Step 4: Re-export the module**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod error;
pub mod ffi;
pub mod grb;
```

- [ ] **Step 5: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test grb_smoke 2>&1 | tail -10
```

Expected: `test result: ok. 4 passed; 0 failed`.

If you see linker errors about missing `GrB_LOR_MONOID_BOOL` or `GxB_LOR_LAND_BOOL`, your SuiteSparse:GraphBLAS may be older than 8.0; the constant names changed between v6 and v8. Check the binding output:

```bash
grep -E "LOR_MONOID_BOOL|LOR_LAND_BOOL" target/debug/build/reasoner-closure-*/out/bindings.rs | head
```

If the symbols are different, update `grb.rs` to match the names in the generated bindings.

If you see `error: linking with cc failed` mentioning `_GrB_init`, the library wasn't linked. Re-run `pkg-config --libs GraphBLAS` and verify `-lgraphblas` (lowercase) appears.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/src/grb.rs crates/closure/src/lib.rs crates/closure/tests/grb_smoke.rs
git commit -m "$(cat <<'EOF'
closure: add safe RAII wrapper for Boolean GraphBLAS matrices

BoolMatrix owns a GrB_Matrix handle, exposes from_edges/new/nvals/
mxm_lor_land/or_assign/wait/extract_edges, and frees on Drop. GrB_init
is done lazily once per process via std::sync::Once. wait() forces
materialisation between operations under GrB_NONBLOCKING mode.
EOF
)"
```

---

## Task 5: Newtype aliases and the core `Triple` / `Edge` types

**Files:**
- Create: `crates/closure/src/types.rs`
- Modify: `crates/closure/src/lib.rs`

- [ ] **Step 1: Write the file**

Create `crates/closure/src/types.rs`:

```rust
//! Newtype IDs used across the closure backend.
//!
//! `DictId` is a dictionary-encoded term ID from SPEC-02 (storage). We treat
//! it as opaque here — closure does not know how to decode URIs, only how to
//! count and renumber them. `DenseIdx` is a 0-based row/column index inside
//! a single predicate's renumbered matrix.

/// Dictionary-encoded term ID from SPEC-02. Stable across the lifetime of the
/// store; closure never invents new ones.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct DictId(pub u64);

/// Dense per-predicate row/column index. Local to one matrix; do not mix
/// indices from different predicates.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct DenseIdx(pub u64);

/// Dictionary ID of a predicate.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct PredicateId(pub u64);

/// A subject/object pair within one predicate's extent.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Edge {
    pub s: DictId,
    pub o: DictId,
}

/// A full triple in dictionary IDs.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Triple {
    pub s: DictId,
    pub p: PredicateId,
    pub o: DictId,
}
```

- [ ] **Step 2: Add the module**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod error;
pub mod ffi;
pub mod grb;
pub mod types;
```

- [ ] **Step 3: Verify it builds**

```bash
cargo check -p reasoner-closure 2>&1 | tail -5
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/closure/src/types.rs crates/closure/src/lib.rs
git commit -m "$(cat <<'EOF'
closure: add DictId / DenseIdx / PredicateId / Edge / Triple types
EOF
)"
```

---

## Task 6: Implement `DenseIdMap` (per-predicate dense renumbering)

**Files:**
- Create: `crates/closure/src/dense_id.rs`
- Modify: `crates/closure/src/lib.rs`

This implements F7 from SPEC-05.

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/dense_id.rs`:

```rust
use reasoner_closure::dense_id::DenseIdMap;
use reasoner_closure::types::{DenseIdx, DictId};

#[test]
fn renumbers_in_first_seen_order() {
    let mut m = DenseIdMap::new();
    assert_eq!(m.intern(DictId(42)), DenseIdx(0));
    assert_eq!(m.intern(DictId(7)), DenseIdx(1));
    assert_eq!(m.intern(DictId(42)), DenseIdx(0));
    assert_eq!(m.len(), 2);
}

#[test]
fn round_trips_dict_to_dense_and_back() {
    let mut m = DenseIdMap::new();
    m.intern(DictId(100));
    m.intern(DictId(200));
    m.intern(DictId(300));
    assert_eq!(m.to_dict(DenseIdx(0)), Some(DictId(100)));
    assert_eq!(m.to_dict(DenseIdx(2)), Some(DictId(300)));
    assert_eq!(m.to_dict(DenseIdx(99)), None);
    assert_eq!(m.to_dense(DictId(200)), Some(DenseIdx(1)));
    assert_eq!(m.to_dense(DictId(404)), None);
}

#[test]
fn bulk_intern_pairs_returns_dense_edges() {
    let mut m = DenseIdMap::new();
    let edges = m.intern_edges(&[
        (DictId(10), DictId(20)),
        (DictId(20), DictId(30)),
    ]);
    // 10 -> 0, 20 -> 1, 30 -> 2 (first-seen order).
    assert_eq!(edges, vec![(0u64, 1u64), (1u64, 2u64)]);
    assert_eq!(m.len(), 3);
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test dense_id 2>&1 | tail -5
```

Expected: `unresolved import reasoner_closure::dense_id`.

- [ ] **Step 3: Implement `dense_id.rs`**

Create `crates/closure/src/dense_id.rs`:

```rust
//! Per-predicate dense renumbering of dictionary IDs.
//!
//! GraphBLAS matrices are most cache-efficient when row/column indices are
//! densely packed starting at 0. The storage crate (SPEC-02) gives us sparse
//! `DictId(u64)` values; we maintain a bijection per predicate so the matrix
//! dimension is exactly the number of distinct subjects/objects appearing in
//! that predicate's extent.
//!
//! Stage-1 simplification: the map is rebuilt from scratch at each bulk
//! materialization checkpoint. SPEC-05 risk note "Dense renumbering
//! invalidation" calls this out — incremental invalidation is Stage 2.

use rustc_hash::FxHashMap;

use crate::types::{DenseIdx, DictId};

/// Bijection `DictId <-> DenseIdx`.
#[derive(Default, Clone)]
pub struct DenseIdMap {
    forward: FxHashMap<DictId, DenseIdx>,
    reverse: Vec<DictId>,
}

impl DenseIdMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            forward: FxHashMap::with_capacity_and_hasher(cap, Default::default()),
            reverse: Vec::with_capacity(cap),
        }
    }

    /// Number of distinct dictionary IDs in the map.
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Insert `id` if not present, return its dense index.
    pub fn intern(&mut self, id: DictId) -> DenseIdx {
        if let Some(&dense) = self.forward.get(&id) {
            return dense;
        }
        let dense = DenseIdx(self.reverse.len() as u64);
        self.reverse.push(id);
        self.forward.insert(id, dense);
        dense
    }

    pub fn to_dense(&self, id: DictId) -> Option<DenseIdx> {
        self.forward.get(&id).copied()
    }

    pub fn to_dict(&self, idx: DenseIdx) -> Option<DictId> {
        self.reverse.get(idx.0 as usize).copied()
    }

    /// Intern both endpoints of every edge and return dense `(u64, u64)` pairs
    /// suitable for `GrB_Matrix_build_BOOL`.
    pub fn intern_edges(&mut self, edges: &[(DictId, DictId)]) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(edges.len());
        for &(s, o) in edges {
            let si = self.intern(s).0;
            let oi = self.intern(o).0;
            out.push((si, oi));
        }
        out
    }
}
```

- [ ] **Step 4: Add the module**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod types;
```

- [ ] **Step 5: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test dense_id 2>&1 | tail -5
```

Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/src/dense_id.rs crates/closure/src/lib.rs crates/closure/tests/dense_id.rs
git commit -m "$(cat <<'EOF'
closure: add DenseIdMap for per-predicate dictionary renumbering

Implements SPEC-05 F7. First-seen insertion order, FxHashMap forward
plus Vec reverse, intern_edges helper that returns the dense (u64,u64)
pairs GrB_Matrix_build_BOOL expects.
EOF
)"
```

---

## Task 7: Implement transitive closure via iterated Boolean MxM

**Files:**
- Create: `crates/closure/src/closure/mod.rs`
- Create: `crates/closure/src/closure/transitive.rs`
- Modify: `crates/closure/src/lib.rs`

This implements F3 — the heart of SPEC-05.

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/transitive_closure.rs`:

```rust
use reasoner_closure::closure::transitive::transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

#[test]
fn chain_of_five_produces_complete_upper_triangle() {
    init_once().unwrap();
    // Chain: 0 -> 1 -> 2 -> 3 -> 4. Closure should add (0,2),(0,3),(0,4),
    // (1,3),(1,4),(2,4) for a total of 10 directed edges.
    let m = BoolMatrix::from_edges(5, &[(0,1),(1,2),(2,3),(3,4)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    let edges = mstar.extract_edges().unwrap();

    let expected: Vec<(u64,u64)> = (0..5)
        .flat_map(|i| ((i+1)..5).map(move |j| (i,j)))
        .collect();
    assert_eq!(edges, expected);
    assert_eq!(mstar.nvals().unwrap(), 10);
}

#[test]
fn single_self_loop_is_fixed_point() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(1, &[(0,0)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.extract_edges().unwrap(), vec![(0,0)]);
}

#[test]
fn triangle_cycle_closes_to_full_3x3() {
    init_once().unwrap();
    // 0 -> 1 -> 2 -> 0. Closure: every pair reachable, including self-loops.
    let m = BoolMatrix::from_edges(3, &[(0,1),(1,2),(2,0)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.nvals().unwrap(), 9);
}

#[test]
fn empty_matrix_is_empty_closure() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(4, &[]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.nvals().unwrap(), 0);
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test transitive_closure 2>&1 | tail -5
```

Expected: `unresolved import reasoner_closure::closure`.

- [ ] **Step 3: Implement `closure/mod.rs`**

Create `crates/closure/src/closure/mod.rs`:

```rust
//! Closure algorithms — transitive, sub-class, sub-property.

pub mod transitive;
pub mod schema;
```

- [ ] **Step 4: Implement `closure/transitive.rs`**

Create `crates/closure/src/closure/transitive.rs`:

```rust
//! Transitive closure of a Boolean adjacency matrix via iterated MxM.
//!
//! Algorithm (semi-naïve, repeated squaring would be denser per step but
//! converges in O(log n) iterations; we use the simpler "fold powers"
//! approach which is closer to the SPEC text and easier to reason about):
//!
//! ```text
//!   R   <- M                 // 1-step reachable
//!   F   <- M                 // current frontier
//!   loop:
//!     F'  <- F * M           // extend frontier by one more hop
//!     prev_nnz <- nnz(R)
//!     R  <- R ∨ F'           // accumulate
//!     if nnz(R) == prev_nnz: break
//!     F  <- F'
//! ```
//!
//! For dense graphs this is O(diameter) MxMs. SuiteSparse's automatic
//! hyper/sparse switching keeps the iteration cheap on skewed inputs.
//!
//! Stage-1 SPEC says `M_p* = I ∨ M_p ∨ M_p² ∨ … ∨ M_p^k`. We **omit `I`**
//! (the identity) because OWL 2 RL transitive-property closure (`prp-trp`)
//! does not infer `?x p ?x` for arbitrary `x` — only edges actually reached.
//! The schema closures (`scm-sco`, `scm-spo`) require reflexivity over the
//! class/property *extent*, which is added separately in `schema.rs`.

use crate::error::GrbError;
use crate::grb::BoolMatrix;

/// Compute `M⁺ = M ∨ M² ∨ M³ ∨ …` until fixed point. The identity is **not**
/// included; the result is the *strictly* transitive closure.
///
/// Stage-1 note: we lack a `GrB_Matrix_dup` wrapper, so we initialise both
/// the accumulator (`reach`) and the frontier by round-tripping through
/// `extract_edges` + `from_edges`. This is correct but performs two extra
/// allocations of size nnz(M); fast-path `dup` is a Stage-2 micro-opt.
pub fn transitive_closure(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    if m.nvals()? == 0 {
        return BoolMatrix::new(m.nrows());
    }

    let n = m.nrows();
    let edges = m.extract_edges()?;
    let mut reach = BoolMatrix::from_edges(n, &edges)?;
    let mut frontier = BoolMatrix::from_edges(n, &edges)?;

    loop {
        // Frontier := Frontier * M (one more hop).
        let next_frontier = frontier.mxm_lor_land(m)?;
        if next_frontier.nvals()? == 0 {
            break;
        }
        let prev_nvals = reach.nvals()?;
        reach.or_assign(&next_frontier)?;
        reach.wait()?; // force materialisation before reading nvals under GrB_NONBLOCKING
        if reach.nvals()? == prev_nvals {
            // No new edges contributed — fixed point.
            break;
        }
        frontier = next_frontier;
    }

    Ok(reach)
}

/// Build an `n x n` identity matrix in Boolean. Only used internally for
/// schema closure (where reflexivity is required).
pub fn identity_like(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    let n = m.nrows();
    let diag: Vec<(u64, u64)> = (0..n).map(|i| (i, i)).collect();
    BoolMatrix::from_edges(n, &diag)
}
```

- [ ] **Step 5: Add the module to the crate root**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod closure;
pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod types;
```

- [ ] **Step 6: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test transitive_closure 2>&1 | tail -10
```

Expected: `test result: ok. 4 passed; 0 failed`.

If `triangle_cycle_closes_to_full_3x3` fails with `nvals = 6` or similar, the `reach.wait()?;` call in `transitive_closure` is missing or in the wrong place — SuiteSparse:GraphBLAS uses lazy evaluation under `GrB_NONBLOCKING` mode, so nvals reads see stale data unless preceded by `GrB_Matrix_wait`. Verify the body shown above includes `reach.wait()?;` immediately after `reach.or_assign(&next_frontier)?;`.

If you see a compile error about `GrB_WaitMode_GrB_MATERIALIZE` not being found, your bindgen output may have generated the enum variant with a slightly different name. Check:

```bash
grep -E "WaitMode|MATERIALIZE|COMPLETE" target/debug/build/reasoner-closure-*/out/bindings.rs | head -5
```

and adjust the variant name in `BoolMatrix::wait` to match.

- [ ] **Step 7: Commit**

```bash
git add crates/closure/src/closure/ crates/closure/src/lib.rs crates/closure/tests/transitive_closure.rs
git commit -m "$(cat <<'EOF'
closure: implement transitive closure via iterated Boolean MxM

Semi-naive frontier-expansion: F := F * M, R := R ∨ F, terminate
when nvals(R) stops growing. Identity is intentionally not included
in the strict transitive closure (prp-trp does not infer x p x).
Schema closure adds reflexivity separately.
EOF
)"
```

---

## Task 8: Implement schema closure (sub-class / sub-property with reflexivity)

**Files:**
- Create: `crates/closure/src/closure/schema.rs`

This implements F2 + the OWL 2 RL rules `scm-sco` and `scm-spo`.

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/schema_closure.rs`:

```rust
use reasoner_closure::closure::schema::reflexive_transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

#[test]
fn sco_chain_includes_reflexivity_over_extent() {
    init_once().unwrap();
    // 3 classes in a chain: A <- B <- C (subClassOf edges B->A, C->B).
    let m = BoolMatrix::from_edges(3, &[(1, 0), (2, 1)]).unwrap();
    let rtc = reflexive_transitive_closure(&m).unwrap();
    let edges = rtc.extract_edges().unwrap();
    // Strict closure: (1,0), (2,0), (2,1). Plus reflexive (0,0),(1,1),(2,2).
    let mut expected: Vec<(u64, u64)> = vec![(0,0),(1,0),(1,1),(2,0),(2,1),(2,2)];
    expected.sort();
    assert_eq!(edges, expected);
}

#[test]
fn empty_input_yields_only_diagonal() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(4, &[]).unwrap();
    let rtc = reflexive_transitive_closure(&m).unwrap();
    assert_eq!(rtc.nvals().unwrap(), 4);
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test schema_closure 2>&1 | tail -5
```

Expected: `unresolved import`.

- [ ] **Step 3: Implement `schema.rs`**

Create `crates/closure/src/closure/schema.rs`:

```rust
//! Schema closures for OWL 2 RL `scm-sco` (rdfs:subClassOf) and `scm-spo`
//! (rdfs:subPropertyOf). Both are **reflexive** transitive closures over
//! the extent of the matrix — every class is a subclass of itself, every
//! property is a sub-property of itself.

use crate::closure::transitive::{identity_like, transitive_closure};
use crate::error::GrbError;
use crate::grb::BoolMatrix;

/// `M* = I ∨ M⁺`. Reflexive transitive closure over `0..n`.
///
/// Use this for `rdfs:subClassOf` (`scm-sco`) and `rdfs:subPropertyOf`
/// (`scm-spo`). Do **not** use for general transitive properties
/// (`prp-trp`) — those use `transitive_closure` directly.
pub fn reflexive_transitive_closure(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    let mut closure = transitive_closure(m)?;
    let identity = identity_like(m)?;
    closure.or_assign(&identity)?;
    closure.wait()?;
    Ok(closure)
}
```

- [ ] **Step 4: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test schema_closure 2>&1 | tail -5
```

Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/closure/src/closure/schema.rs crates/closure/tests/schema_closure.rs
git commit -m "$(cat <<'EOF'
closure: add reflexive_transitive_closure for scm-sco / scm-spo

OWL 2 RL schema rules require reflexivity: every class is a subClassOf
itself. transitive_closure stays strict (matches prp-trp semantics);
this wrapper unions in the identity.
EOF
)"
```

---

## Task 9: Implement union-find based `owl:sameAs` equivalence classes

**Files:**
- Create: `crates/closure/src/sameas.rs`
- Modify: `crates/closure/src/lib.rs`

This implements F4. Pure Rust — no GraphBLAS for EQREL in Stage 1 per the task brief.

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/sameas.rs`:

```rust
use reasoner_closure::sameas::EquivClasses;
use reasoner_closure::types::DictId;

#[test]
fn singletons_are_their_own_representatives() {
    let mut ec = EquivClasses::new();
    ec.insert(DictId(1));
    ec.insert(DictId(2));
    assert_eq!(ec.canonical(DictId(1)), Some(DictId(1)));
    assert_eq!(ec.canonical(DictId(2)), Some(DictId(2)));
    assert!(!ec.same(DictId(1), DictId(2)));
}

#[test]
fn union_merges_classes_and_picks_min_canonical() {
    let mut ec = EquivClasses::new();
    ec.union(DictId(7), DictId(3));
    ec.union(DictId(3), DictId(5));
    // Canonical of {3,5,7} is min = 3.
    assert_eq!(ec.canonical(DictId(3)), Some(DictId(3)));
    assert_eq!(ec.canonical(DictId(5)), Some(DictId(3)));
    assert_eq!(ec.canonical(DictId(7)), Some(DictId(3)));
    assert!(ec.same(DictId(5), DictId(7)));
}

#[test]
fn unknown_id_returns_none() {
    let ec = EquivClasses::new();
    assert!(ec.canonical(DictId(999)).is_none());
}

#[test]
fn class_iter_lists_all_members() {
    let mut ec = EquivClasses::new();
    ec.union(DictId(10), DictId(20));
    ec.union(DictId(20), DictId(30));
    let mut members: Vec<DictId> = ec.class_members(DictId(20)).collect();
    members.sort();
    assert_eq!(members, vec![DictId(10), DictId(20), DictId(30)]);
}

#[test]
fn one_million_unions_yields_one_class() {
    // Stress: chain unions 0~1~2~...~999_999. Canonical of all is DictId(0).
    let mut ec = EquivClasses::new();
    for i in 0..1_000_000u64 {
        ec.union(DictId(i), DictId(i + 1));
    }
    assert_eq!(ec.canonical(DictId(999_999)), Some(DictId(0)));
    assert_eq!(ec.canonical(DictId(123_456)), Some(DictId(0)));
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test sameas 2>&1 | tail -5
```

Expected: `unresolved import`.

- [ ] **Step 3: Implement `sameas.rs`**

Create `crates/closure/src/sameas.rs`:

```rust
//! `owl:sameAs` equivalence classes via union-find with path compression and
//! union-by-rank. Hand-rolled (no `union-find` crate dependency — the
//! algorithm is 80 lines and we want full control over the canonical-
//! representative choice).
//!
//! Canonical representative = lexicographically smallest `DictId` in the
//! class (SPEC-05 F4). Because `DictId` is `u64` and dictionary encoding
//! preserves URI ordering for interned URIs (SPEC-02 NF3), the smallest
//! dict ID corresponds to the smallest URI when terms are interned in
//! lexicographic order. Stage 1 accepts this; if the storage layer changes
//! to non-monotonic ID assignment, the canonical-selection rule will need
//! a side table mapping `DictId -> sort key`.

use rustc_hash::FxHashMap;

use crate::types::DictId;

/// Internal slot index in the union-find arrays.
type Slot = u32;
const NIL: Slot = u32::MAX;

#[derive(Default)]
pub struct EquivClasses {
    /// `DictId` -> internal slot index.
    index: FxHashMap<DictId, Slot>,
    /// `slot` -> `DictId` value at that slot.
    values: Vec<DictId>,
    /// Parent slot of each element (self-pointer = root).
    parent: Vec<Slot>,
    /// Rank for union-by-rank (height upper bound).
    rank: Vec<u8>,
    /// For each root slot, the current canonical `DictId` (min of class).
    /// Non-root entries hold a stale value and must not be consulted.
    canon: Vec<DictId>,
}

impl EquivClasses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            index: FxHashMap::with_capacity_and_hasher(cap, Default::default()),
            values: Vec::with_capacity(cap),
            parent: Vec::with_capacity(cap),
            rank: Vec::with_capacity(cap),
            canon: Vec::with_capacity(cap),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Ensure `id` exists as a singleton. Returns its slot.
    pub fn insert(&mut self, id: DictId) -> Slot {
        if let Some(&slot) = self.index.get(&id) {
            return slot;
        }
        let slot = self.values.len() as Slot;
        assert!(slot != NIL, "EquivClasses capacity exhausted (2^32 - 1 entries)");
        self.values.push(id);
        self.parent.push(slot);
        self.rank.push(0);
        self.canon.push(id);
        self.index.insert(id, slot);
        slot
    }

    /// Union the classes containing `a` and `b`. Inserts singletons if needed.
    pub fn union(&mut self, a: DictId, b: DictId) {
        let sa = self.insert(a);
        let sb = self.insert(b);
        let ra = self.find(sa);
        let rb = self.find(sb);
        if ra == rb {
            return;
        }
        let (root, child) = match self.rank[ra as usize].cmp(&self.rank[rb as usize]) {
            std::cmp::Ordering::Less => (rb, ra),
            std::cmp::Ordering::Greater => (ra, rb),
            std::cmp::Ordering::Equal => {
                self.rank[ra as usize] = self.rank[ra as usize].saturating_add(1);
                (ra, rb)
            }
        };
        self.parent[child as usize] = root;
        // Merge canonical: min of the two roots' canonicals.
        let merged = std::cmp::min(self.canon[root as usize], self.canon[child as usize]);
        self.canon[root as usize] = merged;
    }

    /// Find the root of `slot`, with path compression.
    fn find(&mut self, slot: Slot) -> Slot {
        let mut cur = slot;
        while self.parent[cur as usize] != cur {
            // Path halving — single-pass, no recursion.
            let parent = self.parent[cur as usize];
            let grand = self.parent[parent as usize];
            self.parent[cur as usize] = grand;
            cur = grand;
        }
        cur
    }

    /// Returns `true` if `a` and `b` are in the same class.
    pub fn same(&mut self, a: DictId, b: DictId) -> bool {
        let sa = match self.index.get(&a) { Some(&s) => s, None => return false };
        let sb = match self.index.get(&b) { Some(&s) => s, None => return false };
        self.find(sa) == self.find(sb)
    }

    /// Canonical representative of `id`'s class (min DictId in class).
    /// Returns `None` if `id` is unknown.
    pub fn canonical(&self, id: DictId) -> Option<DictId> {
        let slot = *self.index.get(&id)?;
        // Walk to root without compression (immutable receiver).
        let mut cur = slot;
        while self.parent[cur as usize] != cur {
            cur = self.parent[cur as usize];
        }
        Some(self.canon[cur as usize])
    }

    /// Iterate over every member of `id`'s class. O(n) where n = total
    /// elements in the EquivClasses (Stage-1 acceptable; Stage-2 may add
    /// per-root member lists if hot).
    pub fn class_members(&self, id: DictId) -> Box<dyn Iterator<Item = DictId> + '_> {
        let target_canon = match self.canonical(id) {
            Some(c) => c,
            None => return Box::new(std::iter::empty()),
        };
        Box::new(self.values.iter().copied().filter(move |&v| {
            self.canonical(v) == Some(target_canon)
        }))
    }
}
```

- [ ] **Step 4: Add the module**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod closure;
pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod sameas;
pub mod types;
```

- [ ] **Step 5: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test sameas --release 2>&1 | tail -5
```

Use `--release` for the 1M-union stress test; debug mode is ~30x slower and would push the test over a minute.

Expected: `test result: ok. 5 passed; 0 failed`. The 1M-union test should finish in well under a second in release.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/src/sameas.rs crates/closure/src/lib.rs crates/closure/tests/sameas.rs
git commit -m "$(cat <<'EOF'
closure: add union-find owl:sameAs equivalence classes

Implements SPEC-05 F4. Hand-rolled union-find with path halving and
union-by-rank; canonical representative = min DictId in class. Pure
Rust — no GraphBLAS for EQREL in Stage 1 per plan brief.
EOF
)"
```

---

## Task 10: Define `TripleSink` and `ClosureBackend` traits

**Files:**
- Create: `crates/closure/src/sink.rs`
- Modify: `crates/closure/src/lib.rs`

This is the boundary that storage (SPEC-02) and the rule engine (SPEC-04) will hook into. Per the plan brief: define both traits here, SPEC-04 will depend on us.

- [ ] **Step 1: Write the failing test**

Create `crates/closure/tests/end_to_end.rs`:

```rust
use std::sync::Mutex;

use reasoner_closure::sink::{ClosureBackend, TripleSink};
use reasoner_closure::types::{DictId, PredicateId, Triple};

/// A `TripleSink` that just accumulates into a Vec. Used by tests until the
/// storage crate provides a real implementation.
#[derive(Default)]
struct VecSink {
    triples: Mutex<Vec<Triple>>,
}

impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> Result<u64, anyhow::Error> {
        let mut guard = self.triples.lock().unwrap();
        let before = guard.len();
        guard.extend(triples);
        Ok((guard.len() - before) as u64)
    }
}

#[test]
fn transitive_predicate_closes_and_writes_back() {
    let sink = VecSink::default();
    let mut backend = reasoner_closure::sink::default_backend();

    // Predicate p = 42; transitive chain 1->2->3->4.
    let p = PredicateId(42);
    let edges = vec![
        (DictId(1), DictId(2)),
        (DictId(2), DictId(3)),
        (DictId(3), DictId(4)),
    ];

    let written = backend
        .close_transitive_predicate(p, &edges, &sink)
        .expect("close transitive predicate");

    // Asserted = 3, closure adds (1,3),(1,4),(2,4) = 3 new. Writeback inserts
    // the *full* closure (the backend does not yet diff against asserted —
    // the storage layer is responsible for de-duping on bulk insert).
    assert_eq!(written, 6);

    let triples = sink.triples.lock().unwrap();
    assert_eq!(triples.len(), 6);
    let pairs: Vec<(u64, u64)> = triples.iter().map(|t| (t.s.0, t.o.0)).collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    let expected: Vec<(u64, u64)> = vec![
        (1, 2), (1, 3), (1, 4),
        (2, 3), (2, 4),
        (3, 4),
    ];
    assert_eq!(sorted, expected);
    for t in triples.iter() {
        assert_eq!(t.p, p);
    }
}
```

- [ ] **Step 2: Run the test (should fail to compile)**

```bash
cargo test -p reasoner-closure --test end_to_end 2>&1 | tail -5
```

Expected: `unresolved import reasoner_closure::sink`.

- [ ] **Step 3: Implement `sink.rs`**

Create `crates/closure/src/sink.rs`:

```rust
//! Boundary traits between SPEC-05 and its neighbours.
//!
//! - `TripleSink` is implemented by SPEC-02 (storage). It receives bulk
//!   inserts of inferred triples and must NOT re-fire SPEC-04 rules on them
//!   (avoid infinite re-derivation — F5 in SPEC-05).
//!
//! - `ClosureBackend` is consumed by SPEC-04 (rule engine). The engine routes
//!   the closure subset (prp-trp, scm-sco, scm-spo, eq-*) here instead of
//!   firing those rules itself.

use anyhow::Result;

use crate::closure::schema::reflexive_transitive_closure;
use crate::closure::transitive::transitive_closure;
use crate::dense_id::DenseIdMap;
use crate::grb::{init_once, BoolMatrix};
use crate::sameas::EquivClasses;
use crate::types::{DictId, DenseIdx, PredicateId, Triple};

/// Implemented by SPEC-02 storage. Receives inferred triples in bulk.
///
/// Implementations MUST:
/// - Tag inserted triples as "GraphBLAS-derived" for provenance (SPEC-05 F5).
/// - Skip the SPEC-04 rule-firing path so we do not re-derive what we just
///   materialised (SPEC-05 F5; SPEC-04 F2 codegen must respect this flag).
pub trait TripleSink: Sync {
    /// Bulk-insert inferred triples. Returns the count actually inserted
    /// (after the sink's own de-duplication against existing data).
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> Result<u64>;
}

/// Consumed by SPEC-04 rule engine. The rule engine compiles `prp-trp`,
/// `scm-sco`, `scm-spo`, and `eq-*` rule bodies into calls against this
/// trait rather than into native Datalog clauses.
pub trait ClosureBackend {
    /// Close a transitive predicate over its asserted edges and write the
    /// inferred edges (including the asserted ones, for the simple Stage-1
    /// path) into `sink` as `Triple { s, p, o }`.
    ///
    /// Returns the number of triples reported written by `sink`.
    fn close_transitive_predicate(
        &mut self,
        p: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Close `rdfs:subClassOf` (reflexive transitive closure) and write the
    /// closure as `Triple { s = subclass, p = subclassof_pid, o = superclass }`.
    fn close_subclass(
        &mut self,
        subclassof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Close `rdfs:subPropertyOf` (reflexive transitive closure).
    fn close_subproperty(
        &mut self,
        subpropertyof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Union all asserted `owl:sameAs` pairs into the equivalence-class
    /// structure. Does NOT emit triples — SPARQL/SPEC-04 consult the
    /// structure directly via `equiv_classes()`.
    fn add_sameas(&mut self, pairs: &[(DictId, DictId)]);

    /// Borrow the current equivalence-class state.
    fn equiv_classes(&self) -> &EquivClasses;
}

/// The default `ClosureBackend` we provide. Internally holds a per-predicate
/// `DenseIdMap` and a single `EquivClasses` for sameAs.
pub struct BackendImpl {
    sameas: EquivClasses,
}

impl Default for BackendImpl {
    fn default() -> Self {
        // Cheap & safe to call repeatedly.
        let _ = init_once();
        Self { sameas: EquivClasses::new() }
    }
}

impl ClosureBackend for BackendImpl {
    fn close_transitive_predicate(
        &mut self,
        p: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        if edges.is_empty() {
            return Ok(0);
        }
        let (matrix, map) = build_matrix(edges)?;
        let closure = transitive_closure(&matrix)?;
        let dense_edges = closure.extract_edges()?;
        write_closure(p, &dense_edges, &map, sink)
    }

    fn close_subclass(
        &mut self,
        subclassof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        close_reflexive(subclassof_pid, edges, sink)
    }

    fn close_subproperty(
        &mut self,
        subpropertyof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        close_reflexive(subpropertyof_pid, edges, sink)
    }

    fn add_sameas(&mut self, pairs: &[(DictId, DictId)]) {
        for &(a, b) in pairs {
            self.sameas.union(a, b);
        }
    }

    fn equiv_classes(&self) -> &EquivClasses {
        &self.sameas
    }
}

fn close_reflexive(
    p: PredicateId,
    edges: &[(DictId, DictId)],
    sink: &dyn TripleSink,
) -> Result<u64> {
    if edges.is_empty() {
        return Ok(0);
    }
    let (matrix, map) = build_matrix(edges)?;
    let closure = reflexive_transitive_closure(&matrix)?;
    let dense_edges = closure.extract_edges()?;
    write_closure(p, &dense_edges, &map, sink)
}

fn build_matrix(edges: &[(DictId, DictId)]) -> Result<(BoolMatrix, DenseIdMap)> {
    let mut map = DenseIdMap::with_capacity(edges.len() * 2);
    let dense = map.intern_edges(edges);
    let n = map.len() as u64;
    let m = BoolMatrix::from_edges(n, &dense)?;
    Ok((m, map))
}

fn write_closure(
    p: PredicateId,
    dense_edges: &[(u64, u64)],
    map: &DenseIdMap,
    sink: &dyn TripleSink,
) -> Result<u64> {
    let mut iter = dense_edges.iter().filter_map(|&(s, o)| {
        let s_dict = map.to_dict(DenseIdx(s))?;
        let o_dict = map.to_dict(DenseIdx(o))?;
        Some(Triple { s: s_dict, p, o: o_dict })
    });
    sink.bulk_insert_inferred(&mut iter)
}

/// Convenience constructor for callers (SPEC-04 will use this until it has
/// its own factory).
pub fn default_backend() -> BackendImpl {
    BackendImpl::default()
}
```

- [ ] **Step 4: Add the module**

Update `crates/closure/src/lib.rs`:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.

pub mod closure;
pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod sameas;
pub mod sink;
pub mod types;
```

- [ ] **Step 5: Run the test (should pass)**

```bash
cargo test -p reasoner-closure --test end_to_end 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/src/sink.rs crates/closure/src/lib.rs crates/closure/tests/end_to_end.rs
git commit -m "$(cat <<'EOF'
closure: define TripleSink and ClosureBackend traits + default impl

TripleSink is the storage-side boundary; SPEC-02 implements it and
must tag inserts as GraphBLAS-derived to suppress SPEC-04 re-firing.
ClosureBackend is the rule-engine-side boundary; SPEC-04 routes
prp-trp / scm-sco / scm-spo / eq-* through it.

BackendImpl provides the default end-to-end path: dense renumber ->
GraphBLAS closure -> writeback via the sink.
EOF
)"
```

---

## Task 11: Run the full crate test suite and verify clean build

**Files:** none.

- [ ] **Step 1: Run all tests**

```bash
cargo test -p reasoner-closure --release 2>&1 | tail -30
```

Expected: every test in `grb_smoke`, `dense_id`, `transitive_closure`, `schema_closure`, `sameas`, `end_to_end` passes. Total: ~17 tests.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy -p reasoner-closure --all-targets -- -D warnings 2>&1 | tail -15
```

Expected: no warnings. Fix any that appear before continuing.

- [ ] **Step 3: Verify the workspace still builds**

```bash
cargo check --workspace 2>&1 | tail -5
```

Expected: clean.

- [ ] **Step 4: Commit any clippy fixes**

If clippy required edits, commit them:

```bash
git add -u
git commit -m "$(cat <<'EOF'
closure: clippy cleanup
EOF
)"
```

---

## Task 12: Add criterion benches gated behind feature

**Files:**
- Modify: `crates/closure/Cargo.toml`
- Create: `crates/closure/benches/transitive.rs`
- Create: `crates/closure/benches/sameas.rs`

These drive Stage-1 acceptance criteria 1 and 3.

- [ ] **Step 1: Add criterion as a dev-dep**

Edit `crates/closure/Cargo.toml` to add criterion and re-enable the bench entries:

```toml
[dev-dependencies]
anyhow = { workspace = true }
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "transitive"
harness = false

[[bench]]
name = "sameas"
harness = false
```

- [ ] **Step 2: Write the transitive bench**

Create `crates/closure/benches/transitive.rs`:

```rust
//! Bench: SPEC-05 acceptance criterion 1.
//!
//! "Transitivity-chain benchmark of 2,500 nodes: faster than RDFox by 10×
//!  and faster than GraphDB/OWLIM by 50×."
//!
//! Stage-1 reduced goal: simply finish, and demonstrate the closure is
//! faster than the naive rule-firing baseline (the rule engine does not
//! exist yet in Stage 1, so we measure absolute throughput here and gate
//! the comparative claim in Stage 2).

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use reasoner_closure::closure::transitive::transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

fn chain_matrix(n: u64) -> BoolMatrix {
    let edges: Vec<(u64, u64)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    BoolMatrix::from_edges(n, &edges).unwrap()
}

fn bench_transitive_chain(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("transitive_chain");
    group.measurement_time(Duration::from_secs(20));

    for &n in &[100u64, 500, 2_500] {
        // Closure of an n-chain produces n*(n-1)/2 edges.
        group.throughput(Throughput::Elements((n * (n - 1) / 2) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let m = chain_matrix(n);
            b.iter(|| {
                let star = transitive_closure(&m).unwrap();
                assert_eq!(star.nvals().unwrap(), n * (n - 1) / 2);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_transitive_chain);
criterion_main!(benches);
```

- [ ] **Step 3: Write the sameas bench**

Create `crates/closure/benches/sameas.rs`:

```rust
//! Bench: SPEC-05 acceptance criterion 3.
//!
//! "owl:sameAs equivalence classes on a synthetic graph of 10M sameAs
//!  assertions across 1M canonical entities: union-find construction ≤5 s;
//!  canonical-representative lookup ≤100 ns average."

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::{Rng, SeedableRng};
use rand::rngs::SmallRng;

use reasoner_closure::sameas::EquivClasses;
use reasoner_closure::types::DictId;

fn synth_pairs(n_assertions: usize, n_canonical: u64, seed: u64) -> Vec<(DictId, DictId)> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut pairs = Vec::with_capacity(n_assertions);
    // Each assertion: pick two ids in [0, 10*n_canonical), random.
    let range = 10 * n_canonical;
    for _ in 0..n_assertions {
        let a = rng.gen_range(0..range);
        let b = rng.gen_range(0..range);
        pairs.push((DictId(a), DictId(b)));
    }
    pairs
}

fn bench_sameas_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("sameas_construction");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(10);

    for &(n_assert, n_canon) in &[
        (100_000usize, 10_000u64),
        (1_000_000, 100_000),
        (10_000_000, 1_000_000), // SPEC-05 acceptance criterion 3
    ] {
        let pairs = synth_pairs(n_assert, n_canon, 0xC0FFEE);
        group.throughput(Throughput::Elements(n_assert as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{n_assert}@{n_canon}")),
            &pairs,
            |b, pairs| {
                b.iter(|| {
                    let mut ec = EquivClasses::with_capacity(n_canon as usize * 11);
                    for &(a, b) in pairs {
                        ec.union(a, b);
                    }
                    ec
                });
            },
        );
    }
    group.finish();
}

fn bench_canonical_lookup(c: &mut Criterion) {
    let pairs = synth_pairs(1_000_000, 100_000, 0xBEEF);
    let mut ec = EquivClasses::with_capacity(2_000_000);
    for &(a, b) in &pairs {
        ec.union(a, b);
    }
    // After construction, all parent pointers eventually compress.
    // Warm by walking once.
    for &(a, _) in pairs.iter().take(1000) {
        let _ = ec.canonical(a);
    }
    let probes: Vec<DictId> = pairs.iter().take(10_000).map(|p| p.0).collect();

    let mut group = c.benchmark_group("sameas_lookup");
    group.throughput(Throughput::Elements(probes.len() as u64));
    group.bench_function("canonical_x10k", |b| {
        b.iter(|| {
            let mut sum: u64 = 0;
            for id in &probes {
                if let Some(c) = ec.canonical(*id) {
                    sum = sum.wrapping_add(c.0);
                }
            }
            sum
        });
    });
    group.finish();
}

criterion_group!(benches, bench_sameas_construction, bench_canonical_lookup);
criterion_main!(benches);
```

- [ ] **Step 4: Add `rand` to dev-deps**

Edit `crates/closure/Cargo.toml`:

```toml
[dev-dependencies]
anyhow = { workspace = true }
criterion = { version = "0.5", features = ["html_reports"] }
rand = "0.8"
```

- [ ] **Step 5: Compile-check the benches**

```bash
cargo bench -p reasoner-closure --no-run 2>&1 | tail -10
```

Expected: both `transitive` and `sameas` build successfully.

- [ ] **Step 6: Run the benches (optional, slow)**

```bash
cargo bench -p reasoner-closure 2>&1 | tail -40
```

Expected (rough, reference workstation):
- `transitive_chain/2500`: closure produces 2,500 × 2,499 / 2 = 3,123,750 edges. Stage-1 target: under 1 second per iteration (the SPEC-05 NF1 throughput target of ≥10 M triples/sec at this size means ≤313 ms; we accept anything under 1 s for Stage 1).
- `sameas_construction/10000000@1000000`: under 5 s per sample (SPEC-05 acceptance criterion 3).
- `sameas_lookup/canonical_x10k`: total time / 10k probes should be ≤100 ns per probe.

If any of these wildly miss the target, file an issue and continue — Stage 1 exit is "Stage 1 demonstrates the architecture is sound," not "all NF targets hit." Stage 2 has dedicated tuning passes.

- [ ] **Step 7: Commit**

```bash
git add crates/closure/Cargo.toml crates/closure/benches/
git commit -m "$(cat <<'EOF'
closure: add criterion benches for transitive closure and sameAs

Drives SPEC-05 acceptance criteria 1 (2,500-node chain closure) and 3
(10M sameAs assertions across 1M canonicals + sub-100ns lookup).
Stage 1 records baseline numbers; Stage 2 tunes against them.
EOF
)"
```

---

## Task 13: Add an internal differential consistency test against a naive Rust closure

**Files:**
- Create: `crates/closure/tests/differential.rs`

This is the Stage-1 stand-in for SPEC-05 acceptance criterion 4 ("closure-via-GraphBLAS produces the identical set of inferred triples as closure-via-SPEC-04-rule-firing"). SPEC-04 does not exist yet, so we differentially test against a naive in-Rust Floyd-Warshall-style closure.

- [ ] **Step 1: Write the test**

Create `crates/closure/tests/differential.rs`:

```rust
//! Differential test: GraphBLAS closure vs naive Rust reference closure.
//!
//! Stand-in for SPEC-05 acceptance criterion 4 until SPEC-04 (rule engine)
//! exists to provide the canonical reference. The naive reference here is
//! Floyd–Warshall over a dense Boolean matrix — slow but obviously correct.

use std::collections::BTreeSet;

use rand::{Rng, SeedableRng};
use rand::rngs::SmallRng;

use reasoner_closure::closure::transitive::transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

fn naive_closure(n: usize, edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    let mut reach = vec![vec![false; n]; n];
    for &(s, o) in edges {
        reach[s as usize][o as usize] = true;
    }
    // Floyd–Warshall over Booleans.
    for k in 0..n {
        for i in 0..n {
            if !reach[i][k] { continue; }
            for j in 0..n {
                if reach[k][j] {
                    reach[i][j] = true;
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    for i in 0..n {
        for j in 0..n {
            if reach[i][j] {
                out.insert((i as u64, j as u64));
            }
        }
    }
    out
}

fn random_edges(n: usize, density_per_node: usize, seed: u64) -> Vec<(u64, u64)> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut set: BTreeSet<(u64, u64)> = BTreeSet::new();
    for s in 0..n {
        for _ in 0..density_per_node {
            let o = rng.gen_range(0..n);
            set.insert((s as u64, o as u64));
        }
    }
    set.into_iter().collect()
}

#[test]
fn random_graphs_match_naive_closure() {
    init_once().unwrap();
    for (seed, n, density) in [
        (1u64, 10usize, 2usize),
        (2, 20, 3),
        (3, 50, 4),
        (4, 100, 2),
    ] {
        let edges = random_edges(n, density, seed);
        let naive = naive_closure(n, &edges);

        let m = BoolMatrix::from_edges(n as u64, &edges).unwrap();
        let star = transitive_closure(&m).unwrap();
        let grb: BTreeSet<(u64, u64)> = star.extract_edges().unwrap().into_iter().collect();

        assert_eq!(
            grb, naive,
            "mismatch on seed={seed} n={n} density={density}\n\
             only in grb: {:?}\nonly in naive: {:?}",
            grb.difference(&naive).collect::<Vec<_>>(),
            naive.difference(&grb).collect::<Vec<_>>()
        );
    }
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p reasoner-closure --test differential --release 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`.

If it fails on any random seed, the GraphBLAS closure has a real bug — debug by reducing `n` and printing the symmetric difference set.

- [ ] **Step 3: Commit**

```bash
git add crates/closure/tests/differential.rs
git commit -m "$(cat <<'EOF'
closure: add differential test vs naive Floyd-Warshall closure

Stage-1 stand-in for SPEC-05 acceptance criterion 4 (canonical
reference is SPEC-04 rule firing, which does not exist yet). Random
graphs across multiple sizes and densities must produce identical
reachability sets.
EOF
)"
```

---

## Task 14: Document the Stage-1 surface and Future-Work deferrals

**Files:**
- Modify: `crates/closure/src/lib.rs`

- [ ] **Step 1: Replace the crate-level doc comment**

Open `crates/closure/src/lib.rs` and replace the existing doc comment block at the top with:

```rust
//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.
//!
//! # Stage-1 surface
//!
//! Public API consumed by SPEC-04 (rule engine) and SPEC-02 (storage):
//!
//! - [`sink::ClosureBackend`] — trait the rule engine calls into to close
//!   `prp-trp`, `scm-sco`, `scm-spo`, and to push `owl:sameAs` unions.
//! - [`sink::TripleSink`] — trait the storage layer implements to receive
//!   inferred triples in bulk. The sink MUST tag these as
//!   "GraphBLAS-derived" so the rule engine does not re-fire on them
//!   (SPEC-05 F5).
//! - [`sink::BackendImpl`] / [`sink::default_backend`] — the concrete
//!   implementation we ship.
//! - [`sameas::EquivClasses`] — directly consultable by SPEC-04 and SPEC-07
//!   (SPARQL) for `owl:sameAs` resolution instead of scanning materialised
//!   `eq-*` triples.
//!
//! # Implementation notes
//!
//! - Boolean `(∨, ∧)` semiring closure via iterated [`grb::BoolMatrix::mxm_lor_land`].
//! - Per-predicate dense renumbering via [`dense_id::DenseIdMap`] (SPEC-05 F7);
//!   rebuilt from scratch at each bulk checkpoint (incremental invalidation
//!   is Stage 2).
//! - `owl:sameAs` is pure-Rust union-find (no GraphBLAS); canonical
//!   representative = min `DictId` in class.
//!
//! # Future work (NOT in Stage 1)
//!
//! - Incremental update (SPEC-05 F6): forward-/backward-reachable slice
//!   recomputation on single-edge insertion; integrates with SPEC-06.
//! - GPU GraphBLAS backend: SPEC-09.
//! - LAGraph adoption: Stage 2 evaluation.
//! - Cost-aware closures via `(min, +)` semiring: not required by OWL 2 RL.
//! - Heuristic routing back to direct rule firing when `nnz(M_p) < 10⁴`:
//!   needs benchmark tuning, deferred to Stage 2 (see SPEC-05 risks).
//! - `GrB_Matrix_dup`-based fast clone in the wrapper; current code rebuilds
//!   via `extract_edges` + `from_edges` which is correct but extra-allocating.

pub mod closure;
pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod sameas;
pub mod sink;
pub mod types;
```

- [ ] **Step 2: Verify rustdoc renders**

```bash
cargo doc -p reasoner-closure --no-deps 2>&1 | tail -10
```

Expected: `Documenting reasoner-closure v0.0.0 ...` followed by `Generated /Users/.../target/doc/reasoner_closure/index.html`. No warnings about broken intra-doc links.

If any intra-doc link is broken (e.g. `[grb::BoolMatrix::mxm_lor_land]` typo), fix it before committing.

- [ ] **Step 3: Commit**

```bash
git add crates/closure/src/lib.rs
git commit -m "$(cat <<'EOF'
closure: document Stage-1 public surface and future-work deferrals

Names the SPEC-05 functional requirements implemented in Stage 1 (F1-F5,
F7) and explicitly defers F6 (incremental update) to SPEC-06, GPU to
SPEC-09, and LAGraph to Stage 2.
EOF
)"
```

---

## Acceptance — Stage-1 exit checklist

Run all of the following and confirm green. The first three are hard gates; the bench numbers are reported but not gating.

- [ ] **All unit + integration tests pass**

```bash
cargo test -p reasoner-closure --release
```

- [ ] **Clippy clean**

```bash
cargo clippy -p reasoner-closure --all-targets -- -D warnings
```

- [ ] **Workspace still builds**

```bash
cargo check --workspace
```

- [ ] **2,500-node transitive chain benchmark runs**

```bash
cargo bench -p reasoner-closure --bench transitive
```

Record the wall time for `transitive_chain/2500` in the Stage-1 numbers log. Target: well under the naïve-rule-firing baseline (no SPEC-04 yet, so this is a baseline only).

- [ ] **10M sameAs across 1M canonicals benchmark runs**

```bash
cargo bench -p reasoner-closure --bench sameas
```

Record `sameas_construction/10000000@1000000` wall time. Target: ≤5 s per sample (SPEC-05 acceptance criterion 3). Record `sameas_lookup/canonical_x10k` per-probe time. Target: ≤100 ns.

- [ ] **Differential test passes**

```bash
cargo test -p reasoner-closure --test differential --release
```

Stand-in for SPEC-05 acceptance criterion 4. Once SPEC-04 exists, add a second differential test against rule-fired closure on LUBM-100.

---

## Future work (Stage 2 / SPEC-06 / SPEC-09)

These are intentionally **NOT** in this plan. They are listed here so the next planning pass knows what to pick up:

1. **F6 — Incremental closure update.** On insertion of `(s, p, o)` into a transitive predicate, recompute only the forward-reachable-from-`o` ∪ backward-reachable-to-`s` slice and union the result. Requires integration with SPEC-06's Z-set delta machinery. Deletion uses DBSP, not DRed.
2. **GPU SuiteSparse:GraphBLAS backend (SPEC-09).** CUDA / ROCm. The Davis 2023 GPU code is research-grade; track upstream maturity before adopting.
3. **LAGraph adoption.** Some of our `transitive_closure` reduces to LAGraph's `LAGr_ConnectedComponents` / `LAGr_Reachable` primitives. Evaluate in Stage 2.
4. **`SNOMED CT` benchmark (acceptance criterion 2).** ≤2 s on the reference workstation. Needs the harness (SPEC-01) to load SNOMED CT subClassOf graph.
5. **Memory ratio benchmark (acceptance criterion 5).** Closure of LUBM-8000 transitive properties at ≤2× the asserted transitive triples. Needs LUBM-8000 load (depends on SPEC-02 bulk import).
6. **`GrB_Matrix_dup`-backed clone in the wrapper.** Eliminates the extract_edges / from_edges rebuild path in `transitive_closure`.
7. **Routing heuristic.** Skip GraphBLAS for very small closures (`nnz(M_p) < 10⁴`); route to direct rule firing instead. Threshold needs measurement.
8. **`GxB_set` hints for hypersparse.** Profile and tune for skewed predicates.
9. **Integration with SPEC-04's `prp-*` rules.** When `prp-trp` fires on `?a p ?b ; ?b p ?c`, the rule body must consult `EquivClasses` to also infer for sameAs-equivalent subjects/objects. Interface is the trait already defined here; SPEC-04 plan will exercise it.
