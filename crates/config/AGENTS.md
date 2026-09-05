# `horndb-config` (SPEC-26) — agent notes

Foundation crate for the operator configuration system. Loads one typed
`ServerConfig` by layering (lowest→highest): built-in defaults < base
`config.toml` < `config.d/*.toml` drop-ins < env vars (`HORNDB_` prefix,
`__` nesting) < caller command-line overrides. `figment` is an internal
detail; the public API is `load(&LoadInputs) -> Result<ServerConfig, _>`.

- Model: `src/model.rs` — plain serde structs, `#[serde(deny_unknown_fields, default)]`.
- Units: `src/units.rs` — `ByteSize` (`"2GiB"`), `HumanDuration` (`"30s"`).
- `[server].config_dirs` is a *list* of drop-in dirs (default one entry). Fragments
  from all dirs are pooled and applied in filename order; directory position only
  breaks exact-filename ties (later dir wins). Lets a manual dir and a k8s-operator
  dir coexist. A `config_dirs` set inside a fragment does not relocate the dirs.
- Live reload: `src/watch.rs`. `ConfigHandle` is an `ArcSwap<ServerConfig>` plus a
  generation counter; `watch(inputs, handle)` arms a `notify` watcher, debounces by
  `[reload].debounce`, then re-runs the whole `load` and republishes. Any settled
  event re-resolves everything, so reload is idempotent and event-shape-agnostic.
- The watcher watches **directories** (the base file's parent, plus each
  `config_dirs` entry), never the file itself: an editor's rename-into-place save
  swaps the inode and would orphan a file watch. Do not "optimize" this to a file
  watch — `tests/watch.rs` fails if you do.
- `restart_only_changes(old, new)` lists what a reload stores but cannot apply
  (`[server].bind`, `.config_dirs`, `.shutdown_drain`, the three `[server.limits]`
  admission keys, `[simd]`, `[reasoning]`). The watcher logs each one as "requires
  restart to take effect" and never re-applies `[simd]`.
- Metrics live in `crates/metrics/src/config.rs`; `docs/metrics.md` must move in the
  same commit as any change to them.
- `serve` wiring and `[simd]` injection live in `crates/sparql` (PLAN-26-02), not here.
