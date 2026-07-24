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
- `[simd]` is restart-only; the reload watcher (SPEC-26 S3, later phase) never touches it.
- `serve` wiring and `[simd]` injection live in `crates/sparql` (PLAN-26-02), not here.
