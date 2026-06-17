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
conformance run with the real engine. `workflows/nightly.yml` runs LDBC SPB-256 on a
self-hosted runner.
