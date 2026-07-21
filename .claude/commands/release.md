# Release HornDB

You are performing a **manual** release of HornDB. Arguments: $ARGUMENTS

HornDB carries one version under `[workspace.package]` in the root `Cargo.toml`;
every member crate inherits it via `version.workspace = true` (`crates/python`
is outside the workspace and keeps its own). This command bumps that single
version, curates the changelog, commits, and tags `vX.Y.Z`.

This is the manual path. Do **not** also apply a `bump-*` label to the release
PR — that would trigger the CI auto-bump (`bump-version-on-merge.yml`) on top of
this one. Manual release and the label-driven CI bump are mutually exclusive.

## Step 1: Determine the new version

**Explicit version wins.** If `$ARGUMENTS` contains a bare version number (e.g. `1.2.0`, `v1.2.0`, or `--version 1.2.0`), use it verbatim as the new version — skip the bump-level logic below. A leading `v` is stripped. Sanity-check it is a valid `X.Y.Z` semver and higher than the current version; if it looks like a downgrade or an odd jump, confirm with the user before proceeding.

Otherwise, parse `$ARGUMENTS` for `--bump {major,minor,patch}` (or a bare `major`/`minor`/`patch`). If none is given, determine the level automatically:

1. Find the latest version tag: `git tag --sort=-v:refname | head -1`
2. Get the commits since that tag: `git log <tag>..HEAD --oneline`
3. Classify:
   - **minor**: if there are any `feat:` commits (new features, new SPARQL/OWL surface, new CLI/metrics, new public API)
   - **patch**: if only `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`, `perf:`, or similar non-feature commits
   - **NEVER bump major automatically.** A major bump is always a human decision — only proceed with major if the user explicitly passed it as an argument.

State the bump level and why.

## Step 2: Ensure documentation is up to date

Before writing the changelog, skim the commits since the last tag and confirm the user-facing docs reflect them. Keep this light — flag stale docs, don't rewrite the world:

- **README.md** — new features, CLI flags, SPARQL/OWL surface, or changed usage.
- **docs/architecture.md** — Status fields for any subsystem whose state changed (this is the "current state" view; it should already be in sync per the repo's same-commit rule).
- **docs/** and public API docs — any surface a downstream user touches.

If everything is current, say so and move on. Do not treat internal-only docs (`TASKS.md`, plans, INTEGRATION-NOTES) as release blockers — they track outstanding work, not the release surface.

## Step 3: Write the changelog entry

First check whether `CHANGELOG.md` already has entries under `## [Unreleased]`. If it does, start from those — they were written during development. Supplement with any commits not yet covered.

If `[Unreleased]` is empty, review the commits since the last tag and write the entry from scratch.

Format as [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). One line per change, using these prefixes (no group headings):

- `- Added:` — new features
- `- Changed:` — changes in existing behavior
- `- Improved:` — a material performance improvement
- `- Fixed:` — bug fixes
- `- Removed:` — removed features

**Keep it tidy and user-facing. Short and sweet — aim for tweet-sized lines (~140 characters).**

- Write for someone who *uses* HornDB, not someone who builds it. A user cares about query behavior, conformance, performance, and API — not about how the sausage was made.
- **Cut the fluff.** No CI changes, dependency bumps, internal refactors, test-only changes, doc tweaks, or task bookkeeping. If a line wouldn't matter to a user, drop it.
- Fold multiple commits describing one user-visible change into a single line. Rephrase and reorganize freely — the changelog is not a git log.
- Prefer plain language and the everyday word (per the repo writing-style rules). Say what changed and why it matters, briefly.

Do NOT include the version header line — the release script adds it. The script also clears the `[Unreleased]` section automatically, so don't worry about duplication.

## Step 4: Confirm with user

Show the user:
- Current version → new version
- The bump level and reasoning
- The changelog entry

Ask for confirmation before proceeding. If the user wants changes, revise accordingly.

## Step 5: Run the release script

Once confirmed, pipe the changelog entry to the release script. It sets the workspace version, syncs `Cargo.lock`, updates `CHANGELOG.md`, commits, and tags. Use a heredoc so quotes and special characters survive the shell.

The version source is the first argument: a bump level (`major`/`minor`/`patch`) to bump relative to the current version, or an explicit version (`X.Y.Z` or `vX.Y.Z`). Flags `--bump <level>` / `--version <X.Y.Z>` also work.

```bash
python3 .claude/scripts/release.py <level|version> <<'CHANGELOG'
<changelog entry>
CHANGELOG
```

Pass `--dry-run` first if you want to preview the version bump without writing anything.

## Step 6: Report result

Show the user the new version tag and remind them to push with tags when ready:

```
git push && git push --tags
```

Note: pushing the release commit to `main` also lets CI's `tag-version.yml` see the version change, but it is idempotent — since this script already created `vX.Y.Z`, CI will find the tag exists and do nothing.
