# `.github/` — agent notes

## GitHub Actions hygiene

Pin every GitHub Action to a **full 40-char commit SHA**, never a tag:
`uses: owner/action@<sha> # vX.Y.Z`. The trailing `# vX.Y.Z` comment is required —
it is what a human reads and what Dependabot rewrites on a bump. Floating tags
(`@v4`, `@main`) are a supply-chain risk (the tag can be repointed at malicious
code) and must not appear in `workflows/`. Resolve the SHA first:

```bash
gh api repos/<owner>/<repo>/commits/<tag> --jq .sha   # SHA to pin
```

`dependabot.yml` keeps these pinned SHAs (and their version comments) and the Cargo
workspace dependencies up to date weekly — GitHub Actions updates grouped under a
`ci:` prefix, Cargo minor/patch updates under `chore:`. Review and merge those PRs
like any other; do not hand-bump pins outside that flow unless patching an urgent CVE.

## CI overview

`workflows/ci.yml` mirrors the local fmt + clippy + workspace build plus a
conformance run with the real engine, split into three parallel build jobs so no
compile pipeline waits behind another: `lint` (fmt + workspace clippy + ml
server-feature clippy), `tests` (nextest, doctests), and `conformance` (harness
built once with `--features real-engine` under the `conformance` cargo profile —
release-like but cheaply optimized, see root `Cargo.toml` — then the real-engine
conformance run + junit report). The tests job compiles with
`RUSTFLAGS=-D warnings`, so plain rustc warnings still fail it even without
clippy. All three run every build script, so all three carry the
vendored-GraphBLAS cache (same key — first run after a submodule bump builds it
in each job, then all hit). If branch protection lists required checks, it must
name **all** jobs (`lint`, `tests`, `conformance`, `python-rdflib-compat`); jobs
skipped by the gate count as satisfied. `workflows/nightly.yml` runs LDBC
SPB-256 on a self-hosted runner.

Two cost controls, added after cache analysis (2026-07): a workflow-level
`concurrency` group cancels a PR's superseded runs (pushes to `main` are never
cancelled — a merge burst folds into GitHub's single queued run), and the cargo
cache (`cache-save-if` on the toolchain action) is **saved only from `main`**.
PR runs restore `main`'s cache but do not write their own: per-ref saves
(~1.3 GB per job) were evicting everything out of the repo's 10 GB Actions
cache quota within the hour, so every run compiled from cold. Consequence: a
cold cache on a PR means `main` has not run CI since the cache key last moved
(lockfile / toolchain / runner-image change) — fix it by letting the next
`main` push repopulate, not by re-enabling PR saves.

### CI gate (who may run the build)

`ci.yml` opens with a cheap `gate` job; the build jobs `needs: gate` and run only
when `gate.outputs.build == 'true'`. A push to `main` always builds. A pull request
builds only when its author is a code owner (read from `.github/CODEOWNERS` at the
**base** commit — never the PR's own copy) or a maintainer has applied the
`can-be-tested` label. Applying labels needs Triage+ on the repo, so a fork author
cannot self-authorise. `ci.yml` therefore also listens for the `labeled` PR event so
adding the label re-triggers the run. Only simple `@user` code owners are resolved
(the repo uses `* @stigsb`); `@org/team` owners would need a membership lookup.

The gate also skips the build for **docs-only PRs**: when every changed file is
markdown (`*.md`) or under `docs/`, nothing the build jobs verify can have
changed, so `build=false` and the ~35 runner-minutes are saved. The
`can-be-tested` label overrides this — apply it to force a full build on a
docs-only PR. Pushes to `main` always build regardless of content, both to
validate `main` itself and because only `main` runs save the cargo cache.

### Version bump + tag

The workspace carries one version under `[workspace.package]` in the root
`Cargo.toml`; every member crate inherits it via `version.workspace = true`
(`crates/python` is outside the workspace and keeps its own). Label a PR
`bump-major` / `bump-minor` / `bump-patch`; on merge, `bump-version-on-merge.yml`
runs `scripts/bump-version.py`, commits `Bump version: …` to `main`, and tags
`vX.Y.Z` inline. No bump label → no bump. `tag-version.yml` is the tagging safety
net for version changes that reach `main` via a human-authored merge (a
GITHUB_TOKEN push does not trigger it). Both borrowed/adapted from
`sunstoneinstitute/claude-plugins`.
