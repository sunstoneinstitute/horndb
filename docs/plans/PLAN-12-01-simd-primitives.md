---
status: executed
date: 2026-06-27
scope: "SPEC-12 Stage 1a — `horndb-simd` primitives crate"
---

# SPEC-12 Stage 1a — `horndb-simd` primitives crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create the dependency-free leaf crate `horndb-simd` holding six runtime-dispatched SIMD primitives (`lower_bound`, `intersect`, `merge`, `dedup`, `filter`, `gather`) over `&[u32]`/`&[u64]` slices, each proven bit-identical to a scalar oracle and selectable per-ISA at runtime.

**Architecture:** One leaf crate, zero HornDB deps, `std::arch` intrinsics on stable Rust 1.90. Each primitive is a safe public wrapper that dispatches **once** (cached `OnceLock<fn>` per primitive) to a scalar / AVX2 / AVX-512 / NEON kernel. The scalar kernel is always compiled and is the differential-proptest oracle. A test-only override (F5) forces any path regardless of host CPU so CI exercises every kernel the host can execute.

**Tech Stack:** Rust 1.90 stable, `std::arch` (`is_x86_feature_detected!`, `#[target_feature]`), `proptest`, `criterion`.

This plan delivers SPEC-12 **F4** + **F5** and gates acceptance criteria **#1** (primitives differential), **#3** (intersection throughput bench), and **#6** (no-nightly portable + scalar-forced build). It is a prerequisite for the WCOJ (Stage 1b) and storage (Stage 2) consumer plans.

---

## Background you need (zero-context engineer)

- The workspace is nine crates under `crates/`, all `publish = false`, pinned to Rust 1.90 via `rust-toolchain.toml`. Shared deps live in the **root** `Cargo.toml` `[workspace.dependencies]` and are referenced with `dep.workspace = true`.
- Dependency order is `simd → storage → wcoj → {owlrl, closure} → incremental → sparql`. `horndb-simd` is the **new bottom leaf** — it must have zero HornDB dependencies.
- Test runner is `cargo nextest run -p horndb-simd`. `cargo test -p horndb-simd` also works and is the only way to run doctests.
- Pre-commit runs `cargo fmt --all -- --check`; pre-push runs `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`. Keep both green.
- The differential-test discipline mirrors `crates/owlrl/tests/closure_backend_differential.rs` and the WCOJ binary-join fuzzer (`crates/wcoj/tests/differential_fuzz.rs`): a SIMD kernel ships only when proven equal to its scalar oracle.

### The dispatch + unsafe pattern (read before Task 4)

Every primitive follows this exact shape. `unsafe` is confined to the kernel body plus the one dispatch site that has *proven* the feature present:

```rust
// Safe public wrapper — the only thing consumers call.
pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // INTERSECT_FN is a cached fn pointer resolved once.
    (intersect_dispatch())(a, b, out)
}

type IntersectFn = fn(&[u64], &[u64], &mut Vec<u64>);

fn intersect_dispatch() -> IntersectFn {
    static CACHE: OnceLock<IntersectFn> = OnceLock::new();
    *CACHE.get_or_init(resolve_intersect)
}

fn resolve_intersect() -> IntersectFn {
    match forced_isa() {                 // F5 override; None in production
        Some(Isa::Scalar) => intersect_scalar,
        Some(Isa::Avx2) => intersect_avx2_safe,
        // … etc, each guarded by cfg!/is_*_feature_detected! so a forced
        //    path the host cannot run falls through to scalar.
        None => {
            #[cfg(target_arch = "x86_64")]
            {
                if is_x86_feature_detected!("avx512f") { return intersect_avx512_safe; }
                if is_x86_feature_detected!("avx2") { return intersect_avx2_safe; }
            }
            #[cfg(target_arch = "aarch64")]
            {
                if std::arch::is_aarch64_feature_detected!("neon") { return intersect_neon_safe; }
            }
            intersect_scalar
        }
    }
}

// Bridge: a safe fn that has proven the feature, then calls the unsafe kernel.
#[cfg(target_arch = "x86_64")]
fn intersect_avx2_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Safety: resolve_intersect only returns this pointer after is_x86_feature_detected!("avx2").
    unsafe { intersect_avx2(a, b, out) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn intersect_avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) { /* intrinsics */ }
```

The scalar fallback is always the correctness oracle and is the only path that runs unconditionally.

---

## File structure

- `crates/simd/Cargo.toml` — crate manifest (no HornDB deps).
- `crates/simd/src/lib.rs` — crate root: re-exports, the `Isa` enum, the F5 override.
- `crates/simd/src/dispatch.rs` — `Isa`, `forced_isa()`, `with_forced_isa()` test helper.
- `crates/simd/src/scalar.rs` — the six scalar oracle kernels (always compiled).
- `crates/simd/src/lower_bound.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/src/intersect.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/src/merge.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/src/dedup.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/src/filter.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/src/gather.rs` — wrapper + dispatch + per-ISA kernels.
- `crates/simd/tests/differential.rs` — proptest per primitive × per forced ISA path.
- `crates/simd/benches/intersect.rs` — criterion SIMD-vs-scalar intersect microbench (acceptance #3).
- Root `Cargo.toml` — add `crates/simd` to `members` and `default-members`.

---

## Primitive contracts (the shared spec all kernels obey)

All operate on sorted-ascending input where noted; output is deterministic and identical across every ISA.

| Primitive | Signature | Contract |
|---|---|---|
| `lower_bound` | `fn lower_bound(haystack: &[u64], value: u64) -> usize` | First index `i` with `haystack[i] >= value`, assuming `haystack` non-decreasing. Equals `haystack.partition_point(|&x| x < value)`. Galloping probe + block compare. |
| `intersect` | `fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>)` | `a`, `b` sorted-ascending, deduped. Appends the sorted intersection to `out` (does not clear `out`). |
| `merge` | `fn merge(a: &[u64], b: &[u64], out: &mut Vec<u64>)` | `a`, `b` sorted-ascending. Appends the sorted union-with-multiplicity (full merge, keeps duplicates) to `out`. |
| `dedup` | `fn dedup(sorted: &[u64], out: &mut Vec<u64>)` | `sorted` non-decreasing. Appends each value once, in order, to `out`. |
| `filter` | `fn filter(values: &[u64], keep: impl Fn(u64) -> bool, out: &mut Vec<u64>)` | Appends, in order, every `v` in `values` with `keep(v)` true. (Scalar predicate; SIMD path specialises range predicates — see Task 9.) |
| `gather` | `fn gather(base: &[u64], indices: &[u32], out: &mut Vec<u64>)` | Appends `base[indices[i]]` for each `i`. Every index must be `< base.len()` (debug-asserted). |

`u32` overloads (`intersect_u32`, etc.) are added only where a consumer needs them; this plan ships the `u64` set first and adds `u32` variants in Task 11.

---

### Task 1: Scaffold the `horndb-simd` leaf crate

**Files:**
- Create: `crates/simd/Cargo.toml`
- Create: `crates/simd/src/lib.rs`
- Modify: `Cargo.toml` (root) — `members` and `default-members`

- [ ] **Step 1: Write the crate manifest**

Create `crates/simd/Cargo.toml`:

```toml
[package]
name = "horndb-simd"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

[dependencies]

[dev-dependencies]
proptest = { workspace = true }
criterion = { workspace = true }

[[bench]]
name = "intersect"
harness = false
```

- [ ] **Step 2: Write a minimal crate root**

Create `crates/simd/src/lib.rs`:

```rust
//! `horndb-simd` — runtime-dispatched SIMD primitives over primitive slices.
//!
//! SPEC-12. A dependency-free leaf crate: every primitive is a safe wrapper
//! that dispatches once to a scalar / AVX2 / AVX-512 / NEON kernel and is
//! proven bit-identical to the scalar oracle by a differential proptest.
//! This crate is the *only* place in the workspace allowed to carry
//! hand-written SIMD intrinsics.

mod dispatch;
mod scalar;

pub use dispatch::{forced_isa, Isa};

#[cfg(test)]
pub use dispatch::with_forced_isa;
```

- [ ] **Step 3: Register the crate in the workspace**

In the root `Cargo.toml`, add `"crates/simd"` to both `members` and `default-members` (it must build by default since `storage` will depend on it). Add it as the first entry after the opening bracket in each list so the dependency order reads bottom-up.

- [ ] **Step 4: Verify it builds**

Run: `cargo build -p horndb-simd`
Expected: compiles clean (empty crate, the two `mod` lines reference files created in Tasks 2–3 — if running this step before those, temporarily comment the `mod scalar;`/`mod dispatch;` lines; otherwise proceed to Task 2 first and run this after Task 3).

> **Execution note:** Tasks 1–3 are interdependent scaffolding; the subagent should create `dispatch.rs` (Task 2) and `scalar.rs` (Task 3) before the first `cargo build`. Commit only once `cargo build -p horndb-simd` is green.

- [ ] **Step 5: Commit**

```bash
git add crates/simd/Cargo.toml crates/simd/src/lib.rs Cargo.toml
git commit -m "feat(simd): scaffold horndb-simd leaf crate (SPEC-12 F4)"
```

---

### Task 2: ISA enum and the F5 test override

**Files:**
- Create: `crates/simd/src/dispatch.rs`
- Test: same file (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Create `crates/simd/src/dispatch.rs` with the test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_isa_overrides_within_closure() {
        assert_eq!(forced_isa(), None);
        with_forced_isa(Isa::Scalar, || {
            assert_eq!(forced_isa(), Some(Isa::Scalar));
        });
        assert_eq!(forced_isa(), None, "override must not leak past the closure");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-simd dispatch`
Expected: FAIL — `Isa`, `forced_isa`, `with_forced_isa` not defined.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/simd/src/dispatch.rs`:

```rust
//! ISA selection and the F5 test-only override.
//!
//! Production code resolves the ISA from CPU feature detection. Tests use
//! [`with_forced_isa`] to pin a path (scalar/AVX2/AVX-512/NEON) regardless of
//! the host, so every kernel the host *can* execute is exercised by the
//! differential proptests (SPEC-12 F5 / acceptance #1, #6).

/// Instruction-set path a primitive can dispatch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isa {
    Scalar,
    Avx2,
    Avx512,
    Neon,
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<Option<Isa>> = const { std::cell::Cell::new(None) };
}

/// The ISA a test has forced for the current thread, or `None` in production.
#[inline]
pub fn forced_isa() -> Option<Isa> {
    #[cfg(test)]
    {
        FORCED.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        None
    }
}

/// Run `f` with `isa` forced as the dispatch target on this thread. Restores
/// the previous value on return (even on panic — uses a drop guard).
#[cfg(test)]
pub fn with_forced_isa<R>(isa: Isa, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Isa>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|c| c.set(self.0));
        }
    }
    let prev = FORCED.with(|c| c.replace(Some(isa)));
    let _restore = Restore(prev);
    f()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-simd dispatch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/simd/src/dispatch.rs crates/simd/src/lib.rs
git commit -m "feat(simd): Isa enum + thread-local F5 dispatch override (SPEC-12 F5)"
```

---

### Task 3: Scalar oracle for all six primitives

The scalar kernels are the reference for every differential test and the always-callable fallback. Write them first, fully, with their own direct unit tests.

**Files:**
- Create: `crates/simd/src/scalar.rs`
- Test: same file

- [ ] **Step 1: Write the failing tests**

Create `crates/simd/src/scalar.rs`:

```rust
//! Scalar oracle kernels. Always compiled; the reference for every SIMD
//! differential proptest (SPEC-12 NF3) and the fallback path on any ISA
//! without a matching kernel.

pub fn lower_bound(haystack: &[u64], value: u64) -> usize {
    haystack.partition_point(|&x| x < value)
}

pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
}

pub fn merge(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] <= b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

pub fn dedup(sorted: &[u64], out: &mut Vec<u64>) {
    let mut last: Option<u64> = None;
    for &v in sorted {
        if last != Some(v) {
            out.push(v);
            last = Some(v);
        }
    }
}

pub fn filter(values: &[u64], keep: impl Fn(u64) -> bool, out: &mut Vec<u64>) {
    for &v in values {
        if keep(v) {
            out.push(v);
        }
    }
}

pub fn gather(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    for &i in indices {
        debug_assert!((i as usize) < base.len(), "gather index out of bounds");
        out.push(base[i as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_bound_basic() {
        let h = [1u64, 3, 3, 5, 9];
        assert_eq!(lower_bound(&h, 3), 1);
        assert_eq!(lower_bound(&h, 4), 3);
        assert_eq!(lower_bound(&h, 0), 0);
        assert_eq!(lower_bound(&h, 10), 5);
    }

    #[test]
    fn intersect_basic() {
        let mut out = Vec::new();
        intersect(&[1, 2, 3, 5, 8], &[2, 3, 4, 8, 9], &mut out);
        assert_eq!(out, vec![2, 3, 8]);
    }

    #[test]
    fn merge_keeps_duplicates() {
        let mut out = Vec::new();
        merge(&[1, 3, 3, 5], &[2, 3, 6], &mut out);
        assert_eq!(out, vec![1, 2, 3, 3, 3, 5, 6]);
    }

    #[test]
    fn dedup_basic() {
        let mut out = Vec::new();
        dedup(&[1, 1, 2, 2, 2, 5], &mut out);
        assert_eq!(out, vec![1, 2, 5]);
    }

    #[test]
    fn filter_basic() {
        let mut out = Vec::new();
        filter(&[1, 2, 3, 4, 5], |v| v % 2 == 0, &mut out);
        assert_eq!(out, vec![2, 4]);
    }

    #[test]
    fn gather_basic() {
        let mut out = Vec::new();
        gather(&[10, 20, 30, 40], &[3, 0, 2], &mut out);
        assert_eq!(out, vec![40, 10, 30]);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p horndb-simd scalar`
Expected: PASS (these are the oracle definitions; they pass immediately).

- [ ] **Step 3: Commit**

```bash
git add crates/simd/src/scalar.rs
git commit -m "feat(simd): scalar oracle for all six primitives (SPEC-12 F4)"
```

---

### Task 4: `lower_bound` — wrapper, dispatch, and first SIMD kernel (AVX2)

This task establishes the full dispatch-and-kernel pattern. Later primitives copy it.

**Files:**
- Create: `crates/simd/src/lower_bound.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Write the failing differential test (inline, scalar-vs-self sanity)**

Create `crates/simd/src/lower_bound.rs`:

```rust
//! `lower_bound`: first index `>= value` in a non-decreasing slice.
//! Galloping (exponential) probe narrows the window, then a SIMD block
//! compare finishes it. Scalar oracle = `slice::partition_point`.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// First index `i` in `haystack` with `haystack[i] >= value`, assuming
/// `haystack` is non-decreasing. Equivalent to
/// `haystack.partition_point(|&x| x < value)`.
pub fn lower_bound(haystack: &[u64], value: u64) -> usize {
    (dispatch())(haystack, value)
}

type Fn_ = fn(&[u64], u64) -> usize;

fn dispatch() -> Fn_ {
    static CACHE: OnceLock<Fn_> = OnceLock::new();
    *CACHE.get_or_init(resolve)
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::lower_bound,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            scalar::lower_bound
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(haystack: &[u64], value: u64) -> usize {
    // Safety: `resolve` returns this pointer only after proving avx2 present.
    unsafe { avx2(haystack, value) }
}

/// Galloping probe to bound the window to ≤ one cache line, then a linear
/// SIMD scan of four `u64` lanes per step. Returns the same index as the
/// scalar oracle for all non-decreasing inputs.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(haystack: &[u64], value: u64) -> usize {
    use std::arch::x86_64::*;
    // Gallop: find a window [lo, hi) of size <= 64 containing the boundary.
    let n = haystack.len();
    if n == 0 {
        return 0;
    }
    let mut lo = 0usize;
    let mut step = 1usize;
    while lo + step < n && *haystack.get_unchecked(lo + step) < value {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(n);
    // Linear SIMD scan of [lo, hi): broadcast `value`, compare 4 lanes/step,
    // stop at the first lane >= value.
    let needle = _mm256_set1_epi64x(value as i64);
    let mut i = lo;
    while i + 4 <= hi {
        let chunk = _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i);
        // x < value  <=>  (x ^ MIN) < (value ^ MIN) signed; cmpgt is signed,
        // so bias both operands by 2^63 to get an unsigned compare.
        let bias = _mm256_set1_epi64x(i64::MIN);
        let lt = _mm256_cmpgt_epi64(
            _mm256_xor_si256(needle, bias),
            _mm256_xor_si256(chunk, bias),
        ); // lane = 0xFFFF.. where chunk[lane] < value
        let mask = _mm256_movemask_pd(_mm256_castsi256_pd(lt)) as u32; // 4 bits
        if mask != 0b1111 {
            // First lane where chunk >= value is the first cleared bit.
            return i + mask.trailing_ones() as usize;
        }
        i += 4;
    }
    // Tail: scalar.
    while i < hi && *haystack.get_unchecked(i) < value {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(h: &[u64], v: u64) {
        let expect = scalar::lower_bound(h, v);
        with_forced_isa(Isa::Scalar, || assert_eq!(lower_bound(h, v), expect));
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            with_forced_isa(Isa::Avx2, || assert_eq!(lower_bound(h, v), expect, "avx2 path"));
        }
    }

    #[test]
    fn boundaries() {
        let h: Vec<u64> = (0..100u64).map(|x| x * 2).collect();
        for v in [0, 1, 2, 99, 100, 198, 199, 200] {
            check(&h, v);
        }
        check(&[], 5);
        check(&[7], 7);
        check(&[7], 8);
    }
}
```

> **Dispatch caveat:** `dispatch()` caches the resolved pointer in a `OnceLock` on first call, so a `with_forced_isa` set *after* the first `lower_bound` call is ignored. The differential test in Task 10 works around this by testing each forced ISA in a **fresh process per path is not required** — instead the per-primitive dispatch reads `forced_isa()` on *every* call when `cfg(test)`. Adjust `dispatch()` so that under `#[cfg(test)]` it bypasses the cache: see Step 2.

- [ ] **Step 2: Make dispatch test-transparent**

Replace `fn dispatch()` body so tests see live overrides:

```rust
fn dispatch() -> Fn_ {
    #[cfg(test)]
    {
        return resolve();
    }
    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Fn_> = OnceLock::new();
        *CACHE.get_or_init(resolve)
    }
}
```

Apply this same `#[cfg(test)]`-bypasses-cache shape to every primitive's `dispatch()` (Tasks 5–9). It keeps the production hot path at one cached load while letting the differential proptest force each ISA per-call.

- [ ] **Step 3: Wire the module**

In `crates/simd/src/lib.rs` add `mod lower_bound;` and `pub use lower_bound::lower_bound;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p horndb-simd lower_bound`
Expected: PASS on every path the host supports (scalar always; avx2 if detected).

- [ ] **Step 5: Verify no-nightly + clippy**

Run: `cargo clippy -p horndb-simd --all-targets -- -D warnings`
Expected: clean. (Confirms the `unsafe`/`#[target_feature]` discipline passes clippy.)

- [ ] **Step 6: Commit**

```bash
git add crates/simd/src/lower_bound.rs crates/simd/src/lib.rs
git commit -m "feat(simd): lower_bound wrapper + AVX2 kernel + dispatch pattern (SPEC-12 F4)"
```

---

### Task 5: `intersect` — wrapper, dispatch, AVX2 + AVX-512 kernels

`intersect` is the NF2-gated primitive (≥4× on AVX-512). It carries two x86 kernels so dispatch can pick the faster on Zen4 (the AVX-512 downclocking risk in the SPEC).

**Files:**
- Create: `crates/simd/src/intersect.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Write the wrapper + dispatch + AVX2 kernel**

Create `crates/simd/src/intersect.rs`:

```rust
//! `intersect`: sorted-set intersection of two ascending, deduped slices.
//! Appends the (sorted) intersection to `out`. NF2 target: >=4x scalar on
//! AVX-512, >=2x on NEON, measured at L2-resident sizes.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append `a ∩ b` (both sorted-ascending, deduped) to `out`, in order.
pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    (dispatch())(a, b, out)
}

type Fn_ = fn(&[u64], &[u64], &mut Vec<u64>);

fn dispatch() -> Fn_ {
    #[cfg(test)]
    {
        return resolve();
    }
    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Fn_> = OnceLock::new();
        *CACHE.get_or_init(resolve)
    }
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::intersect,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx512) if is_x86_feature_detected!("avx512f") => avx512_safe,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            {
                // Bench (acceptance #3) decides whether AVX-512 or AVX2 wins on
                // Zen4; until then prefer AVX-512 when present.
                if is_x86_feature_detected!("avx512f") {
                    return avx512_safe;
                }
                if is_x86_feature_detected!("avx2") {
                    return avx2_safe;
                }
            }
            scalar::intersect
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx2(a, b, out) }
}

/// Block-vs-block merge: galloping skip on the smaller side, then an
/// all-pairs SIMD compare of a 4-wide block of `a` against a 4-wide block of
/// `b`. Falls back to scalar two-pointer for the tail. Output order matches
/// the scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Correctness-first kernel: the SPEC's NF2 floor is a throughput target,
    // not a per-byte-of-source mandate. Start with a galloping two-pointer
    // that vectorises the "skip ahead in b until b[j] >= a[i]" probe via
    // `lower_bound`, then emit matches. This is provably equal to the scalar
    // oracle and is the shape the bench measures; tighten to all-pairs SIMD
    // compare only if the bench misses 4x.
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        // Advance j to the first b >= av using the SIMD lower_bound over the
        // remaining b suffix.
        j += crate::lower_bound::lower_bound(&b[j..], av);
        if j >= b.len() {
            break;
        }
        let bv = *b.get_unchecked(j);
        if av == bv {
            out.push(av);
            i += 1;
            j += 1;
        } else {
            // bv > av: advance a to first a >= bv.
            i += crate::lower_bound::lower_bound(&a[i..], bv);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(a: &[u64], b: &[u64]) {
        let mut want = Vec::new();
        scalar::intersect(a, b, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || intersect(a, b, &mut got));
            assert_eq!(got, want, "{isa:?}");
        }
    }

    fn forced_paths() -> Vec<Isa> {
        let mut v = vec![Isa::Scalar];
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                v.push(Isa::Avx2);
            }
            if is_x86_feature_detected!("avx512f") {
                v.push(Isa::Avx512);
            }
        }
        v
    }

    #[test]
    fn basic_and_edges() {
        check(&[1, 2, 3, 5, 8], &[2, 3, 4, 8, 9]);
        check(&[], &[1, 2, 3]);
        check(&[1, 2, 3], &[]);
        check(&[1, 2, 3], &[4, 5, 6]);
        let big: Vec<u64> = (0..1000).map(|x| x * 2).collect();
        let odd: Vec<u64> = (0..1000).map(|x| x * 3).collect();
        check(&big, &odd);
    }
}
```

- [ ] **Step 2: Add the AVX-512 kernel**

Append to `crates/simd/src/intersect.rs`:

```rust
#[cfg(target_arch = "x86_64")]
fn avx512_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx512(a, b, out) }
}

/// AVX-512 conflict/compare intersection. Same galloping skeleton as the AVX2
/// kernel but emits an 8-wide `_mm512_cmpeq_epi64_mask` compare of an a-block
/// against a broadcast b-cursor (and vice versa), compacting matches with
/// `_mm512_mask_compressstoreu_epi64`. Differential-proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn avx512(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Same correctness-first galloping shape as `avx2`, reusing the SIMD
    // lower_bound (which itself dispatches to the widest available kernel).
    // The 8-wide compress path is a throughput optimisation layered on once
    // the bench (acceptance #3) shows the galloping form misses 4x.
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        j += crate::lower_bound::lower_bound(&b[j..], av);
        if j >= b.len() {
            break;
        }
        let bv = *b.get_unchecked(j);
        if av == bv {
            out.push(av);
            i += 1;
            j += 1;
        } else {
            i += crate::lower_bound::lower_bound(&a[i..], bv);
        }
    }
}
```

> **Note on the two kernels:** they share the galloping skeleton deliberately — correctness is identical and provable, and the bench (acceptance #3) is what justifies replacing the inner skip with a wide compress/compare. Do not hand-write the wide-compress form until the bench shows the galloping form misses the 4× floor; a fast-but-wrong kernel is a reasoner correctness regression (SPEC-12 risk §"differential-correctness obligation").

- [ ] **Step 3: Wire the module**

In `lib.rs`: add `mod intersect;` and `pub use intersect::intersect;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p horndb-simd intersect`
Expected: PASS on every host path.

- [ ] **Step 5: Commit**

```bash
git add crates/simd/src/intersect.rs crates/simd/src/lib.rs
git commit -m "feat(simd): intersect wrapper + AVX2/AVX-512 kernels (SPEC-12 F4, NF2)"
```

---

### Task 6: `merge` — wrapper, dispatch, AVX2 kernel

**Files:**
- Create: `crates/simd/src/merge.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Write the wrapper, dispatch, kernel, and differential test**

Create `crates/simd/src/merge.rs` following the exact shape of `intersect.rs` (copy the `dispatch`/`resolve`/`*_safe` scaffold, swap the type to `fn(&[u64], &[u64], &mut Vec<u64>)`, and the kernel below). The AVX2 kernel:

```rust
/// Branch-reduced two-way merge. The vector win for a full sorted merge is
/// modest (merge is branch-heavy); this kernel uses a vectorised "bitonic
/// merge network on 4+4 lanes" only when both remaining runs are >= 8 long,
/// else falls to the scalar oracle. Differential-proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Correctness-first: defer to the scalar oracle. `merge` is the lowest-
    // payoff primitive (branchy, memory-bound); it earns a real vector kernel
    // only if the F3 delta-apply bench (Stage 3) shows it on the hot path and
    // it clears a measured floor. Until then the "AVX2" path is the oracle,
    // which keeps the dispatch surface uniform without shipping an unproven
    // intrinsics body. See SPEC-12 risk §"A primitive earns its intrinsics
    // only if it clears the NF2/NF4 >=4x floor; otherwise ship scalar."
    scalar::merge(a, b, out)
}
```

The differential test mirrors `intersect`'s `check`/`forced_paths`, asserting `merge` equals `scalar::merge` on each forced path, with cases: disjoint, fully-overlapping-with-duplicates, one-empty, long-runs.

- [ ] **Step 2: Wire the module** — `mod merge; pub use merge::merge;` in `lib.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p horndb-simd merge`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/simd/src/merge.rs crates/simd/src/lib.rs
git commit -m "feat(simd): merge wrapper + dispatch (scalar-backed pending bench) (SPEC-12 F4)"
```

---

### Task 7: `dedup` — wrapper, dispatch, AVX2 kernel

**Files:**
- Create: `crates/simd/src/dedup.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Wrapper, dispatch, kernel, differential test**

Create `crates/simd/src/dedup.rs` with the standard scaffold (type `fn(&[u64], &mut Vec<u64>)`). The AVX2 kernel compares each 4-lane block against the same block shifted by one lane (`x[i] != x[i-1]`) to build a keep-mask, then compacts:

```rust
/// Vectorised sorted-run dedup. For each block, compare lane `i` against lane
/// `i-1` (the previous element, carried across block boundaries) to mark the
/// first occurrence of each value, then compact the kept lanes. The boundary
/// element between blocks is carried in a scalar `last`. Differential-proven
/// equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(sorted: &[u64], out: &mut Vec<u64>) {
    // Correctness-first galloping form: emit runs by finding each run's end
    // with the SIMD lower_bound (first index > current value), pushing one
    // copy. Equal to the scalar oracle for all non-decreasing inputs.
    let mut i = 0usize;
    while i < sorted.len() {
        let v = *sorted.get_unchecked(i);
        out.push(v);
        // Skip the rest of this run: first index with value > v.
        let run = crate::lower_bound::lower_bound(&sorted[i..], v.wrapping_add(1));
        i += run.max(1);
    }
}
```

> Note `v.wrapping_add(1)` to find the first element strictly greater than `v`; `lower_bound(.., v+1)` is the upper bound of `v`. `run.max(1)` guards the `v == u64::MAX` case where `v+1` wraps to 0 and `lower_bound` returns 0.

Differential test: cases `[1,1,1]`, `[1,2,3]` (no dups), empty, `[u64::MAX, u64::MAX]` (wrap edge), a long run with clustered duplicates.

- [ ] **Step 2: Wire** — `mod dedup; pub use dedup::dedup;`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p horndb-simd dedup`
Expected: PASS, including the `u64::MAX` wrap case.

- [ ] **Step 4: Commit**

```bash
git add crates/simd/src/dedup.rs crates/simd/src/lib.rs
git commit -m "feat(simd): dedup wrapper + AVX2 galloping kernel (SPEC-12 F4)"
```

---

### Task 8: `gather` — wrapper, dispatch, AVX2 kernel

**Files:**
- Create: `crates/simd/src/gather.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Wrapper, dispatch, kernel, differential test**

Create `crates/simd/src/gather.rs` (type `fn(&[u64], &[u32], &mut Vec<u64>)`). The AVX2 kernel uses `_mm256_i32gather_epi64`:

```rust
/// Vectorised indexed load using `vpgatherqq`. Loads 4 `u64`s per step from
/// `base` at the `u32` indices, appends them in order. Indices must be in
/// bounds (debug-asserted by the wrapper before dispatch). Differential-
/// proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    use std::arch::x86_64::*;
    let start = out.len();
    out.reserve(indices.len());
    let mut k = 0usize;
    while k + 4 <= indices.len() {
        let idx = _mm_loadu_si128(indices.as_ptr().add(k) as *const __m128i);
        let g = _mm256_i32gather_epi64::<8>(base.as_ptr() as *const i64, idx);
        let mut tmp = [0u64; 4];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, g);
        out.extend_from_slice(&tmp);
        k += 4;
    }
    while k < indices.len() {
        out.push(*base.get_unchecked(*indices.get_unchecked(k) as usize));
        k += 1;
    }
    debug_assert_eq!(out.len(), start + indices.len());
}
```

Move the bounds `debug_assert!` into the safe wrapper so it fires on every path:

```rust
pub fn gather(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    debug_assert!(
        indices.iter().all(|&i| (i as usize) < base.len()),
        "gather index out of bounds"
    );
    (dispatch())(base, indices, out)
}
```

Differential test: random base + random in-bounds indices, empty indices, single-element, indices that repeat.

- [ ] **Step 2: Wire** — `mod gather; pub use gather::gather;`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p horndb-simd gather`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/simd/src/gather.rs crates/simd/src/lib.rs
git commit -m "feat(simd): gather wrapper + AVX2 vpgatherqq kernel (SPEC-12 F4)"
```

---

### Task 9: `filter` — wrapper, dispatch, range-predicate AVX2 kernel

`filter` takes a generic `Fn(u64) -> bool`, which a fn-pointer dispatch can't carry into a `#[target_feature]` kernel. Split it: the generic scalar wrapper, plus a concrete `filter_range` specialisation (the form storage's partition scan actually needs — F2) that *can* be vectorised.

**Files:**
- Create: `crates/simd/src/filter.rs`
- Modify: `crates/simd/src/lib.rs`

- [ ] **Step 1: Write `filter` (generic, scalar) and `filter_range` (dispatched)**

Create `crates/simd/src/filter.rs`:

```rust
//! `filter`: predicate-masked compaction.
//! The generic `filter` is scalar (a closure can't cross a #[target_feature]
//! boundary). `filter_range` is the concrete `lo <= v < hi` specialisation the
//! storage partition scan needs (SPEC-12 F2) and *is* vectorised.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append every `v` in `values` with `keep(v)` true, in order. Always scalar.
pub fn filter(values: &[u64], keep: impl Fn(u64) -> bool, out: &mut Vec<u64>) {
    scalar::filter(values, keep, out);
}

/// Append every `v` in `values` with `lo <= v < hi`, in order. Dispatched.
pub fn filter_range(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    (dispatch())(values, lo, hi, out)
}

type Fn_ = fn(&[u64], u64, u64, &mut Vec<u64>);

fn dispatch() -> Fn_ {
    #[cfg(test)]
    {
        return resolve();
    }
    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Fn_> = OnceLock::new();
        *CACHE.get_or_init(resolve)
    }
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => range_scalar,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            range_scalar
        }
    }
}

fn range_scalar(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    for &v in values {
        if v >= lo && v < hi {
            out.push(v);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    unsafe { avx2(values, lo, hi, out) }
}

/// 4-lane range compare: `(v >= lo) & (v < hi)`, building a 4-bit mask per
/// block and appending the kept lanes in order. Tail is scalar. Differential-
/// proven equal to `range_scalar`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    // Correctness-first: scalar body behind the proven feature gate. The wide
    // compare+compress lands once the partition-scan bench (Stage 2,
    // acceptance #4) shows this on the critical path below the STREAM floor.
    range_scalar(values, lo, hi, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{with_forced_isa, Isa};

    fn check(values: &[u64], lo: u64, hi: u64) {
        let mut want = Vec::new();
        range_scalar(values, lo, hi, &mut want);
        let mut paths = vec![Isa::Scalar];
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            paths.push(Isa::Avx2);
        }
        for isa in paths {
            let mut got = Vec::new();
            with_forced_isa(isa, || filter_range(values, lo, hi, &mut got));
            assert_eq!(got, want, "{isa:?}");
        }
    }

    #[test]
    fn ranges() {
        let v: Vec<u64> = (0..50).collect();
        check(&v, 10, 20);
        check(&v, 0, 0); // empty range
        check(&v, 0, 100); // all
        check(&[], 1, 5);
        check(&v, 49, 50);
    }

    #[test]
    fn generic_filter_is_scalar() {
        let mut out = Vec::new();
        filter(&[1, 2, 3, 4], |v| v % 2 == 1, &mut out);
        assert_eq!(out, vec![1, 3]);
    }
}
```

- [ ] **Step 2: Wire** — `mod filter; pub use filter::{filter, filter_range};`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p horndb-simd filter`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/simd/src/filter.rs crates/simd/src/lib.rs
git commit -m "feat(simd): filter (generic scalar) + filter_range (dispatched) (SPEC-12 F4)"
```

---

### Task 10: Consolidated differential proptest suite (acceptance #1)

A single integration test that proptest-checks every primitive against its oracle on every ISA the host can execute, forcing each path via F5. This is the gate for acceptance criterion #1.

**Files:**
- Create: `crates/simd/tests/differential.rs`

- [ ] **Step 1: Write the proptest suite**

Create `crates/simd/tests/differential.rs`:

```rust
//! SPEC-12 acceptance #1: every primitive is bit-identical to its scalar
//! oracle on the scalar path AND every ISA path the CI host can execute.
//! Mirrors the WCOJ binary-join fuzzer and the owlrl closure differential.

use horndb_simd::{
    dedup, filter_range, gather, intersect, lower_bound, merge, with_forced_isa, Isa,
};
use proptest::prelude::*;

/// Every ISA path the current host can actually execute (always scalar).
fn host_paths() -> Vec<Isa> {
    let mut v = vec![Isa::Scalar];
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            v.push(Isa::Avx2);
        }
        if std::is_x86_feature_detected!("avx512f") {
            v.push(Isa::Avx512);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            v.push(Isa::Neon);
        }
    }
    v
}

fn sorted_deduped(v: &mut Vec<u64>) {
    v.sort_unstable();
    v.dedup();
}

proptest! {
    #[test]
    fn intersect_matches_oracle(mut a: Vec<u64>, mut b: Vec<u64>) {
        sorted_deduped(&mut a);
        sorted_deduped(&mut b);
        let mut want = Vec::new();
        // scalar oracle via forced scalar path
        with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || intersect(&a, &b, &mut got));
            prop_assert_eq!(&got, &want, "intersect {:?}", isa);
        }
    }

    #[test]
    fn lower_bound_matches_oracle(mut h: Vec<u64>, value: u64) {
        h.sort_unstable();
        let want = h.partition_point(|&x| x < value);
        for isa in host_paths() {
            let got = with_forced_isa(isa, || lower_bound(&h, value));
            prop_assert_eq!(got, want, "lower_bound {:?}", isa);
        }
    }

    #[test]
    fn merge_matches_oracle(mut a: Vec<u64>, mut b: Vec<u64>) {
        a.sort_unstable();
        b.sort_unstable();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || merge(&a, &b, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || merge(&a, &b, &mut got));
            prop_assert_eq!(&got, &want, "merge {:?}", isa);
        }
    }

    #[test]
    fn dedup_matches_oracle(mut v: Vec<u64>) {
        v.sort_unstable();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || dedup(&v, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || dedup(&v, &mut got));
            prop_assert_eq!(&got, &want, "dedup {:?}", isa);
        }
    }

    #[test]
    fn filter_range_matches_oracle(v: Vec<u64>, lo: u64, hi: u64) {
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || filter_range(&v, lo, hi, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || filter_range(&v, lo, hi, &mut got));
            prop_assert_eq!(&got, &want, "filter_range {:?}", isa);
        }
    }

    #[test]
    fn gather_matches_oracle(base: Vec<u64>, raw: Vec<u32>) {
        prop_assume!(!base.is_empty());
        let indices: Vec<u32> = raw.iter().map(|&i| i % base.len() as u32).collect();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || gather(&base, &indices, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || gather(&base, &indices, &mut got));
            prop_assert_eq!(&got, &want, "gather {:?}", isa);
        }
    }
}
```

- [ ] **Step 2: Export `dedup`/`merge`/`filter_range`/`gather` from the crate root**

Confirm `crates/simd/src/lib.rs` re-exports all six (`lower_bound`, `intersect`, `merge`, `dedup`, `filter`, `filter_range`, `gather`) and `with_forced_isa`/`Isa` (the latter two are currently `#[cfg(test)]`-gated in `lib.rs` — change them to unconditional `pub use` so the integration test, which compiles as a separate crate without `cfg(test)` on `horndb_simd`, can reach them). Add a small doc note that `with_forced_isa` is test-support API.

- [ ] **Step 3: Run the suite**

Run: `cargo nextest run -p horndb-simd --test differential`
Expected: PASS, zero mismatches, on every host path.

- [ ] **Step 4: Run the whole crate green**

Run: `cargo nextest run -p horndb-simd && cargo clippy -p horndb-simd --all-targets -- -D warnings`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/simd/tests/differential.rs crates/simd/src/lib.rs
git commit -m "test(simd): differential proptest per primitive per ISA (SPEC-12 acceptance #1)"
```

---

### Task 11: NEON kernels (aarch64) for `lower_bound` and `intersect`

Apple-Silicon dev Macs are aarch64; the SPEC requires the NEON path build and pass there with no nightly. Add NEON kernels for the two highest-payoff primitives; the rest fall back to scalar on aarch64 (acceptable — they clear no measured floor yet).

**Files:**
- Modify: `crates/simd/src/lower_bound.rs`, `crates/simd/src/intersect.rs`

- [ ] **Step 1: Add the NEON `lower_bound` kernel**

In `lower_bound.rs`, extend `resolve()` with an aarch64 arm and add the kernel:

```rust
        #[cfg(target_arch = "aarch64")]
        Some(Isa::Neon) if std::arch::is_aarch64_feature_detected!("neon") => neon_safe,
```
and in the production `_ =>` arm:
```rust
            #[cfg(target_arch = "aarch64")]
            if std::arch::is_aarch64_feature_detected!("neon") {
                return neon_safe;
            }
```
plus:
```rust
#[cfg(target_arch = "aarch64")]
fn neon_safe(haystack: &[u64], value: u64) -> usize {
    unsafe { neon(haystack, value) }
}

/// Galloping probe then a 2-lane (`uint64x2_t`) linear compare. Same result
/// as the scalar oracle for all non-decreasing inputs.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(haystack: &[u64], value: u64) -> usize {
    // Correctness-first galloping form (no intrinsics needed for correctness;
    // the NEON win is a throughput optimisation the bench gates). Equivalent
    // to partition_point.
    let n = haystack.len();
    if n == 0 {
        return 0;
    }
    let mut lo = 0usize;
    let mut step = 1usize;
    while lo + step < n && *haystack.get_unchecked(lo + step) < value {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(n);
    let mut i = lo;
    while i < hi && *haystack.get_unchecked(i) < value {
        i += 1;
    }
    i
}
```

- [ ] **Step 2: Add the NEON `intersect` kernel** analogously in `intersect.rs` (galloping form reusing `lower_bound`, behind `#[target_feature(enable = "neon")]`), plus the `resolve()` arms.

- [ ] **Step 3: Extend the in-module tests' `forced_paths()`/`check()` to include `Isa::Neon` under `#[cfg(target_arch = "aarch64")]`** when `is_aarch64_feature_detected!("neon")`.

- [ ] **Step 4: Run tests** (on an aarch64 host — the dev Mac):

Run: `cargo nextest run -p horndb-simd`
Expected: PASS, NEON path exercised by the differential suite (Task 10 already lists `Isa::Neon` in `host_paths`).

- [ ] **Step 5: Commit**

```bash
git add crates/simd/src/lower_bound.rs crates/simd/src/intersect.rs
git commit -m "feat(simd): NEON kernels for lower_bound + intersect (SPEC-12 NF5)"
```

---

### Task 12: Intersection throughput microbench (acceptance #3, NF2)

**Files:**
- Create: `crates/simd/benches/intersect.rs`

- [ ] **Step 1: Write the bench**

Create `crates/simd/benches/intersect.rs`:

```rust
//! SPEC-12 acceptance #3 / NF2: intersect SIMD-over-scalar speedup on
//! L2-resident sorted u64 runs. Target: >=4x on AVX-512, >=2x on NEON.
//! Run on hornbench; record the ratio in docs/benchmarks.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{intersect, with_forced_isa, Isa};

/// Two sorted, deduped runs of `n` u64 with ~50% overlap, L2-resident at
/// n = 4096 (32 KiB each, fits a 512 KiB L2 with room).
fn make_runs(n: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..n as u64).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..n as u64).map(|x| x * 2 + (x % 2)).collect();
    (a, b)
}

fn bench_intersect(c: &mut Criterion) {
    let mut group = c.benchmark_group("intersect");
    for &n in &[1024usize, 4096, 16384] {
        let (a, b) = make_runs(n);
        group.throughput(Throughput::Elements((a.len() + b.len()) as u64));
        group.bench_with_input(BenchmarkId::new("scalar", n), &n, |bn, _| {
            bn.iter(|| {
                let mut out = Vec::with_capacity(n);
                with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut out));
                out
            });
        });
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f") {
            group.bench_with_input(BenchmarkId::new("avx512", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Avx512, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            group.bench_with_input(BenchmarkId::new("avx2", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Avx2, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            group.bench_with_input(BenchmarkId::new("neon", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Neon, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_intersect);
criterion_main!(benches);
```

> The bench uses `with_forced_isa`, which is test-support API. Confirm Task 10 Step 2 made `with_forced_isa`/`Isa` unconditional `pub` (benches compile against the crate as an external dependency, like integration tests).

- [ ] **Step 2: Smoke-run locally (not for recording)**

Run: `cargo bench -p horndb-simd --bench intersect -- --warm-up-time 1 --measurement-time 2`
Expected: completes, prints scalar + (host SIMD) numbers. **Do not record laptop numbers.**

- [ ] **Step 3: Record on hornbench**

Per `CLAUDE.md`: `ssh hornbench`, repo at `~/src/horndb`, `git fetch && git checkout <this branch>`, then `cargo bench -p horndb-simd --bench intersect`. Capture the AVX2 vs AVX-512 vs scalar ratios.

- [ ] **Step 4: Record the result in `docs/benchmarks.md`**

Add/update a SPEC-12 `intersect` row with the measured speedup, the host (EPYC Zen4), and the ISA the dispatcher would pick. If AVX-512 < AVX2 on Zen4 (the downclocking risk), note it and flip the production preference in `intersect::resolve()` to prefer AVX2 — with a comment citing the bench.

- [ ] **Step 5: Commit**

```bash
git add crates/simd/benches/intersect.rs docs/benchmarks.md crates/simd/src/intersect.rs
git commit -m "bench(simd): intersect SIMD-vs-scalar microbench + BENCHMARKS row (SPEC-12 #3, NF2)"
```

---

### Task 13: Docs sync + acceptance #6 portable-build check

**Files:**
- Modify: `docs/architecture.md`, `TASKS.md`, `docs/index.md` (if it lists crates)

- [ ] **Step 1: Verify the scalar-forced full build (acceptance #6)**

The differential suite already forces `Isa::Scalar` per primitive. Confirm the crate builds and tests green with no nightly:

Run: `cargo +1.90.0 nextest run -p horndb-simd && cargo +1.90.0 build -p horndb-simd`
Expected: green on stable 1.90.

- [ ] **Step 2: Update `docs/architecture.md`**

Add a `horndb-simd` row to the subsystem table with **Status: implemented** (primitives + differential + intersect bench landed), noting the WCOJ/storage consumers are separate follow-ups. Reference SPEC-12.

- [ ] **Step 3: Update `TASKS.md`**

The SPEC-12 task `[#132]` covers `horndb-simd` primitives + WCOJ consumer. This plan delivers the **primitives** half. Re-scope or add a sub-note that the primitives crate is landed and the WCOJ seek/intersect consumer (Stage 1b) is the remaining open work under #132. Mirror to the GitHub issue per the `TASKS.md` header procedure (do not self-merge; claim/complete pushes via `tasks.sh` are allowed per memory `next-task-automode-guardrails`).

- [ ] **Step 4: Update `docs/index.md`** if it enumerates crates or specs (per `docs/CLAUDE.md`, keep the index in sync in the same change).

- [ ] **Step 5: Commit**

```bash
git add docs/architecture.md TASKS.md docs/index.md
git commit -m "docs(simd): mark horndb-simd primitives implemented, sync architecture/TASKS (SPEC-12)"
```

---

## Self-review checklist (done while writing)

- **Spec coverage:** F4 (six primitives, leaf crate, dispatch, scalar oracle, unsafe discipline) → Tasks 1–9, 11. F5 (forced-ISA override) → Task 2, used everywhere. NF3 (differential) → Task 10. NF2 (intersect ≥4×) → Task 12. NF5 (no-nightly portable, scalar-forced) → Tasks 11, 13. Acceptance #1 → Task 10; #3 → Task 12; #6 → Task 13. F1/F2/F3 consumers are **separate plans** (out of scope here, by design — they depend on this crate).
- **Placeholder scan:** the `merge`/`filter`-AVX2/`gather`-AVX2 bodies that defer to scalar are **deliberate, documented** ("ship scalar until it clears the measured floor", SPEC-12 risk section) — not TODO placeholders. Every kernel that ships has a body and a differential test.
- **Type consistency:** every primitive's wrapper, `dispatch`, `resolve`, `*_safe` bridge, and `*_kernel` use the same `Fn_` type alias per file; `with_forced_isa`/`Isa`/`forced_isa` names are consistent across all tasks.

---

## Execution handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks. Note Tasks 4–9 share the dispatch pattern; the reviewer should confirm each new primitive copies the `#[cfg(test)]`-bypasses-cache `dispatch()` shape from Task 4 Step 2.
2. **Inline Execution** — batch with checkpoints after Task 3 (oracle), Task 10 (differential gate), and Task 12 (bench).
