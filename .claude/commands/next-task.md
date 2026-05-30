---
description: Pick the next open task from TASKS.md, claim it with a [v] in the main worktree, and drive it to PR.
---

# /next-task — claim and drive the next TASKS.md item to a PR

You are running the `/next-task` workflow. Pick the single highest-priority
open task from `TASKS.md`, **claim it** by marking it `[v]` (in-progress) on
`main`, build it in an isolated worktree, and open a PR. Operate in
**fully-autonomous** mode: do not stop for confirmation between phases unless a
preflight check fails or you genuinely cannot proceed safely.

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

## Phase 7 — Close the bookkeeping (same commit)

In the worktree branch, in the **closing commit**:

- **Single-PR task:** flip the claimed line `[v]` → `[x]` on both the index and
  the body heading, **remove the wip tag**, keep the `([#N](url))` link, and
  update the `TASKS.md` index to match.
- **Epic increment:** leave the parent task `[v]`; check off / close only the
  sub-issue's increment. Update the tracking note if useful.
- Flip the matching **Status** field in `docs/architecture.md` (typically
  `planned` → `implemented` when a task is checked off).

Commit message should describe the work; reference the issue.

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

## Phase 9 — Report

Summarise to the user:

- Which task was picked (and why — priority/order), and its `#N`.
- Whether it was treated as an epic (and the sub-issues created, if so).
- The claim commit on `main`, the worktree path, and the branch.
- Verification results (the real command outcomes).
- The PR URL.
- Any follow-ups left open (especially the remaining epic increments).

Leave the worktree in place; the user can remove it with
`git worktree remove .worktrees/<branch>` after the PR merges.
