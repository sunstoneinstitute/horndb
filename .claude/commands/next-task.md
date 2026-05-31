---
description: Pick the next open task from TASKS.md, claim it with a [v] in the main worktree, and drive it through PR, codex review, and merge.
---

# /next-task — claim and drive the next TASKS.md item to a merged PR

You are running the `/next-task` workflow. Pick the single highest-priority
open task from `TASKS.md`, **claim it** by marking it `[v]` (in-progress) on
`main`, build it in an isolated worktree, open a PR, get it **reviewed by
codex** and address the feedback, then **merge** and only then close the
task/issue. Operate in **fully-autonomous** mode: do not stop for confirmation
between phases unless a preflight check fails, codex is unavailable, the
review→fix loop fails to converge, the merge is blocked, or you genuinely
cannot proceed safely.

Honour the project's global rules throughout:

- **Use `superpowers:subagent-driven-development`** to execute the
  implementation (this is the user's standing default — do not ask).
- **Keep the docs in sync** per `CLAUDE.md` → "Keep the docs in sync" and
  "Keep GitHub issues in sync with `TASKS.md`": `TASKS.md`, its index,
  `docs/architecture.md` **Status**, and the matching GitHub issue all move
  together, in the same commit/PR.
- **Verify before claiming done** per
  `superpowers:verification-before-completion` — run the real commands, read
  the real output.

---

## Phase 0 — Preflight (abort on failure)

1. Confirm you are in the **main worktree** on branch `main` with a **clean**
   working tree:
   ```bash
   git rev-parse --show-toplevel
   git branch --show-current      # must be: main
   git status --porcelain         # must be empty
   ```
   If the tree is dirty or you're not on `main`, **stop** and tell the user —
   do not stash or discard their work.
2. Capture identifiers for the claim tag:
   ```bash
   SESSION="${CLAUDE_CODE_SESSION_ID:0:8}"   # short session id
   DATE="$(date +%F)"                          # YYYY-MM-DD
   ```
3. `git fetch origin` and fast-forward `main` so you claim against the latest
   state (avoids racing another agent). Re-read `TASKS.md` after.

## Phase 1 — Select the task

Parse `TASKS.md`. Consider **only open `[ ]` items** — `[v]` (already claimed
by another session) and `[x]` (done) are skipped automatically; that skipping
*is* the anti-collision mechanism, so never pick a `[v]`.

Sort the open items by **priority first** (`CRITICAL` → `HIGH` → `MEDIUM` →
`LOW`), then by **file order** within a priority. Pick the first. This ordering
is explicit so it stays correct even if the index is reordered.

Announce the pick: its title, **priority/category**, and its `([#N](url))`
tracking issue. Then proceed.

If there are **no open `[ ]` tasks**, report that `TASKS.md` is clear and stop.

## Phase 2 — Epic check

Decide honestly: **can this task be completed and merged in a single coherent
PR?** Whole-SPEC items (e.g. "SPEC-07 SPARQL (…)", "SPEC-02 storage (…)") with
broad parenthetical scope usually cannot.

- **Not an epic** → continue to Phase 3 with the task as-is.
- **Epic** → the task's existing `#N` issue *is* the tracker. Break it into
  **shippable increments** and create one **sub-issue per increment**, each
  with matching `priority:` + `category:` labels (see `CLAUDE.md` taxonomy):
  ```bash
  gh issue create --repo sunstoneinstitute/horndb \
    --title "<increment title>" \
    --label "priority: <p>" --label "category: <c>" \
    --body-file <file>
  ```
  Link each sub-issue under the parent `#N`. Prefer real GitHub sub-issue
  linking if available in this environment:
  ```bash
  gh sub-issue add --repo sunstoneinstitute/horndb <N> <child> 2>/dev/null \
    || true   # fall back below if the extension/API is unavailable
  ```
  If sub-issue linking is unavailable, instead edit the parent `#N` body to
  hold a task list of `- [ ] #<child>` references, and reference `#N` in each
  child body. Either way, leave a **link to the tracking issue `#N`** in the
  `TASKS.md` body for this task so the breakdown is discoverable.

  Then **select the first increment's sub-issue** and treat *that* as the unit
  of work for the rest of this run. The parent task line in `TASKS.md` becomes
  `[v]` (in progress, not done) — it only flips to `[x]` once every sub-issue
  is closed.

## Phase 3 — Claim it on `main` (before branching)

Derive a slug and branch name: `branch="task-<N>-<short-kebab-slug>"`.

Edit `TASKS.md` on `main`, on **both** the index line and the body heading for
this task: change `[ ]` → `[v]` and append a **visible trailing tag**:

```
— _wip: session <SESSION> · <branch> · <DATE>_
```

For an epic increment, point the tag at the breakdown, e.g.
`— _wip: session <SESSION> · tracking #<N> · <branch> · <DATE>_`.

Commit and push this claim on `main`:

```bash
git add TASKS.md
git commit -m "chore(tasks): claim #<N> (<title>) [v] — session <SESSION>"
git push origin main
```

Claiming **before** creating the branch is deliberate: the worktree forks from
the `[v]` state, so the later `[v]` → `[x]` flip merges back without a conflict
on that line.

## Phase 4 — Isolated worktree

Create the worktree under **`.worktrees/`** (not `.claude/worktrees/`):

```bash
git worktree add ".worktrees/<branch>" -b "<branch>" main
```

Work from `.worktrees/<branch>`. To avoid recompiling the ~700 MB rocksdb
artifact in a fresh tree, point Cargo at a shared target dir for this run (per
`CLAUDE.md`):

```bash
export CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/../.horndb-shared-target"
```

(Adjust to an existing shared path if the user already has one.)

## Phase 5 — Implement

Inside the worktree, drive the work to completion:

1. Research the relevant SPEC(s), plan(s), `INTEGRATION-NOTES.md`, and the
   task body in `TASKS.md`. Respect each crate's gotchas in `CLAUDE.md`.
2. Use `superpowers:writing-plans` to produce the implementation plan, then
   **`superpowers:subagent-driven-development`** to execute it with atomic
   commits.
3. Honour the harness-first rule (SPEC-00): the task is not satisfied until its
   referenced subset in the harness is green. Grow a subset if needed, never
   bypass it.

## Phase 6 — Verify (real commands, real output)

From the worktree, run and read the actual output:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p horndb-sparql --features server   # if SPARQL is touched
```

Plus any task-specific gate named in `TASKS.md` (e.g. a criterion bench, the
WCOJ differential fuzzer, a harness suite). If a gate is red, fix it before
proceeding — do not claim completion on red.

## Phase 7 — Bookkeeping commit (closure is merge-gated)

In the worktree branch, in the **closing commit**:

- **Single-PR task:** flip the claimed line `[v]` → `[x]` on both the index and
  the body heading, **remove the wip tag**, keep the `([#N](url))` link, and
  update the `TASKS.md` index to match.
- **Epic increment:** leave the parent task `[v]`; mark only the sub-issue's
  increment delivered in the breakdown note. Update the tracking note if useful.
- Flip the matching **Status** field in `docs/architecture.md` (typically
  `planned` → `implemented` when a task is checked off).

Commit message should describe the work; reference the issue.

**Do not close anything yet.** The `[x]` flip lives on the branch, so it lands
on `main` only when the PR merges — that is intentional. Do **not** run
`gh issue close` here. The task and its issue are closed only *after* a
confirmed merge (Phase 11). The whole point of the review→merge loop below is
that nothing is marked done until the merge actually happens.

## Phase 8 — Open the PR

```bash
git push -u origin "<branch>"
```

Open the PR with base `main`. The body must include **`Closes #<N>`** (for an
epic increment, `Closes #<sub-issue>` and reference the parent `#<N>`) so the
merge auto-closes the tracking issue — that closure is the GitHub mirror of the
`[x]` flip required by `CLAUDE.md`. Prefer the project's `new-pr` skill if
available, otherwise:

```bash
gh pr create --repo sunstoneinstitute/horndb --base main \
  --title "<concise title>" --body-file <pr-body-file>
```

Do **not** advertise the assistant in the commit, PR title, or PR body (global
rule). Do not add `Co-authored-by` trailers.

**Do not stop here.** Opening the PR is the *start* of the review→merge loop,
not the end. Continue straight into Phase 9.

## Phase 9 — Review the PR with codex

Get an independent, cross-model review from **codex** (a different model family
than the one that wrote the code — that independence is the value). Run from the
worktree, on the feature branch, reviewing the branch diff against `main`:

```bash
cd ".worktrees/<branch>"
codex review --base main "Review this PR for correctness, security, and \
quality. The task is <one-line task + its #N>. Acceptance criteria: <the \
TASKS.md / SPEC acceptance gates>. Flag bugs, missing requirements, and \
unjustified scope; be concrete with file:line." 2>&1 | tee /tmp/codex-review-<N>.txt
```

Notes and fallbacks:

- `codex review --base main` reviews everything on the branch vs `main`
  non-interactively. (`--commit <sha>` or `--uncommitted` exist if you need a
  narrower scope.)
- If codex is not authenticated (`codex login` required) or not installed,
  **stop and tell the user** — do not silently skip the review or substitute a
  self-review. The review phase is mandatory; the user chose codex specifically
  for cross-model independence.
- Capture the findings verbatim (the `tee` file) so Phase 10 can work through
  them and Phase 12 can summarise them.

## Phase 10 — Address codex's findings with claude (loop)

Work through codex's review **with engineering rigor, not deference** — use the
`superpowers:receiving-code-review` skill. For each finding:

- **Verify it against the code first.** codex can be wrong or miss context.
  Confirm a finding is real before acting; if you disagree, say why (you will
  record the justification in the report) rather than applying a change you
  believe is incorrect.
- **Fix the real ones in the worktree**, with atomic commits (one logical fix
  per commit), and **push**. For non-trivial fixes, prefer
  `superpowers:subagent-driven-development` (the user's standing default);
  trivial one-liners may be applied directly.
- **Re-run the Phase 6 verification** for anything you touched (`fmt`, `clippy`,
  the relevant tests, plus any task-specific gate). Never push a fix on red.
- Keep `TASKS.md` / `docs/architecture.md` / the issue in sync if a fix changes
  scope (same rules as Phase 7).

Then **re-run `codex review --base main`** (Phase 9) to confirm the findings are
resolved and no new ones were introduced. Loop until codex reports no
actionable/blocking findings — or the only remainder are items you have
explicitly, defensibly declined. Cap the loop at ~3 rounds; if it is not
converging, **stop and surface the disagreement to the user** rather than
churning.

## Phase 11 — Merge, then close

Only once the review is clean (or remaining items are justified declines) **and**
all PR checks/CI are green **and** the branch is mergeable:

1. Confirm state:
   ```bash
   gh pr checks <N> --repo sunstoneinstitute/horndb
   gh pr view  <N> --repo sunstoneinstitute/horndb --json mergeable,mergeStateStatus,statusCheckRollup
   ```
2. **Merge** the PR (use the repo's convention; default to squash for a branch
   with TDD micro-commits + review-fix commits):
   ```bash
   gh pr merge <N> --repo sunstoneinstitute/horndb --squash --delete-branch
   ```
   Merging is an outward, hard-to-reverse action — but invoking `/next-task`
   *is* the standing authorization to merge once you are "happy" per the loop
   above. If the merge is blocked (e.g. by an auto-mode permission classifier),
   stop and ask the user to authorize it; do not work around the block.
3. **Now close the bookkeeping** (this is the only place closure happens):
   - The `Closes #<N>` in the PR body auto-closes the tracking issue on merge.
     Verify: `gh issue view <N> --repo sunstoneinstitute/horndb --json state`
     → `CLOSED`. If it did not auto-close, `gh issue close <N>`.
   - The `[x]` flip / `docs/architecture.md` Status change land on `main` with
     the merge (they were in the closing commit). Fast-forward local `main`:
     `git fetch origin && git -C "$(git rev-parse --show-toplevel)" switch main && git merge --ff-only origin/main`.
   - **Epic increment:** the sub-issue is now closed by its `Closes`. If *all*
     sibling sub-issues are closed, flip the parent task `[v]` → `[x]` and
     `gh issue close <parent>`; otherwise leave the parent `[v]` and update its
     breakdown note to mark this increment done.
4. Remove the worktree now that it is merged:
   ```bash
   git worktree remove ".worktrees/<branch>"
   ```

## Phase 12 — Report

Summarise to the user:

- Which task was picked (and why — priority/order), and its `#N`.
- Whether it was treated as an epic (and the sub-issues created, if so).
- The claim commit on `main`, the worktree path, and the branch.
- Verification results (the real command outcomes).
- The PR URL, the **codex review** outcome, and what was changed in response
  (including any findings you defensibly declined, with the reason).
- The **merge** result and confirmation the issue is closed / task `[x]` on
  `main` (or, for an epic, that the parent stays `[v]` with N increments left).
- Any follow-ups left open (especially the remaining epic increments).
