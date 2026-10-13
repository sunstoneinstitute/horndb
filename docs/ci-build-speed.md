# Speeding up Rust CI builds

A record of what we measured and tried to cut HornDB's CI build time, written
so the same steps can be repeated here later or in another Rust repo. Each item
says whether it helped, did nothing, or was not tried, and why.

## Start by measuring

Get per-step times for recent runs before changing anything:

```bash
gh run list -w ci -L 10 --json databaseId,event,conclusion
gh run view <id> --json jobs -q '.jobs[] | .name as $n | .steps[]
  | select(.startedAt!=null)
  | "\($n) \(((.completedAt|fromdate)-(.startedAt|fromdate)))s \(.name)"'
gh cache list -L 30          # what the Actions cache actually holds
```

Then grep a job log for `No cache found` / `Cache not found` and for
`Compiling proc-macro2`. If the low-level crates compile in every run, the
cache is not working, and nothing else matters until it is.

Baseline (2026-10-13, GitHub-hosted `ubuntu-latest`, cold): `clippy` 1306 s,
`nextest` 913 s, conformance job 8 min. The three build jobs run in parallel,
so wall-clock was about 22 min.

## What we found and tried

| Item | Result |
|---|---|
| Cargo cache (`Swatinem/rust-cache`, via `setup-rust-toolchain`) | **Broken. Every run built from cold.** `cache-save-if` saved only on `refs/heads/main`, but with a merge queue every run is `pull_request` or `merge_group`. No `push` run on `main` ever happened, so nothing was ever saved. Fix is open (see Next steps). Check this first in any repo that uses a merge queue. |
| `cargo machete` (unused dependencies) | **Biggest win.** Found `oxigraph` declared in the harness but used by no source file. Its only effect was to pull in `oxrocksdb-sys`, a ~700 MB C++ RocksDB build in every job. Removed, along with six small unused deps. Now runs in the `lint` job so it cannot come back. |
| `cargo machete` false positives | It matches package names in the source text, so a crate imported under another name looks unused (`md-5` is imported as `md5`). Silence each one with `[package.metadata.cargo-machete] ignored = [...]` in that crate's `Cargo.toml`. Always rebuild after removing what it reports. |
| `cargo udeps` | Not tried. Needs a nightly toolchain and a full build; machete was enough. |
| Faster linker (mold / lld) | Not needed here. Rust ≥ 1.90 links with `rust-lld` by default on `x86_64-unknown-linux-gnu`. On older toolchains or other targets, try `-C link-arg=-fuse-ld=lld`. |
| Less debug info | Already done before this pass: `[profile.dev] debug = "line-tables-only"` shrank test binaries by ~70 %, which cuts link time and disk. CI's harness uses a cheap `conformance` profile (`opt-level = 1`, no LTO). |
| Trimming default features | Checked `[workspace.dependencies]`. The heavy ones (`arrow`, `criterion`, `tokio`) were already trimmed. Nothing obvious left; use `--timings` to find a new target. |
| `sccache` | Not used in CI. On persistent runners a persistent target dir does the same job more simply. On GitHub-hosted runners it competes for the same 10 GB cache quota. Useful locally across worktrees (see root `CLAUDE.md`). |
| Merging lint and test jobs | Little gain. `clippy` only type-checks (`.rmeta`), while tests need generated code, so the two share only build-script output. Separate jobs run in parallel, which matters more. |

## Next steps

- Fix the cargo cache: either save from `merge_group` runs, or move the build
  jobs to the self-hosted `gha-buildcache` runners with a persistent
  `CARGO_TARGET_DIR` outside the checkout (the data-platform repo's pattern).
  The second option has no cache quota and no download step, but HornDB is a
  public repo, so fork PRs (gated behind the `can-be-tested` label) would run
  on our host. The target dir also needs pruning, because it grows without
  limit.
- Run `cargo build --timings` once on CI after the cache fix and attach the
  HTML report. It shows which crates dominate the remaining build time.

## Further reading

- The Cargo Book, [Optimizing Build Performance](https://doc.rust-lang.org/cargo/guide/build-performance.html)
- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
