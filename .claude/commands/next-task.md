---
description: Pick the next open task from TASKS.md, claim it with a [v] in the main worktree, and drive it through PR, codex review, and merge.
---

# /next-task — claim and drive the next TASKS.md item to a merged PR

You are running the `/next-task` workflow. A script claims the single
highest-priority open task from `TASKS.md` and hands you a ready worktree;
you build the task, open a PR, get it **reviewed by codex** (if codex is
available in the environment — see Phase 6) and address the feedback, then
**merge** and only then close the task/issue. Operate in **fully-autonomous**
mode: do not stop for confirmation between phases unless the bootstrap fails,
the review→fix loop fails to converge, the merge is blocked, or you genuinely
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

## Coordination — TASKS.md is lock-serialized

Several `/next-task` agents run in parallel against the **same main worktree**,
so they race on `TASKS.md` claim/complete lines. **Every** TASKS.md transition
goes through `.claude/scripts/tasks.sh`: an `flock`-guarded
edit → commit → `push origin main` transaction, run from the main worktree on
`main`, that fast-forwards first and refuses a `claim` whose task is not open
(exit 9 — the anti-collision check). Each claim is stamped
`session@host · branch · UTC-timestamp` so orphaned work is identifiable
(`claims` lists them, `reap --older-than DUR [--apply]` releases stale ones).
Run `.claude/scripts/tasks.sh help` for the full contract.

Consequence: **TASKS.md never appears in a feature branch.** The claim is a
locked commit on `main` (bootstrap); the `[v]`→`[x]` (or epic release) is a
locked commit on `main` *after* the merge (Phase 8). Keeping TASKS.md off
feature branches is what eliminates the cross-agent merge/rebase conflicts.

## Phase 0 — Bootstrap (scripted; replaces manual preflight/select/claim/worktree)

From the main worktree on `main`, run **one command**:

```bash
.claude/scripts/next-task.sh start
```

It deterministically does everything up to the point where implementation
starts — do **not** re-do any of these steps by hand:

1. **Preflight** — refuses linked worktrees, non-`main` branches, and dirty
   tracked files; fetches + fast-forwards `main`.
2. **Orphan report** — prints active claims and a stale-claim dry run
   (>12h; override with `NEXT_TASK_REAP`). It never auto-reaps: if something
   looks orphaned (dead session/host, very old), release it with
   `.claude/scripts/tasks.sh reap --older-than 12h --apply`.
3. **Select + claim** — picks the first open `[ ]` index task ordered
   CRITICAL → HIGH → MEDIUM → LOW, then file order; claims it through
   `tasks.sh` (locked, pushed to `main`); on a lost race it automatically
   retries the next candidate. `--issue N` forces a specific task.
4. **Worktree** — creates `.worktrees/task-<N>-<slug>` forked from the exact
   claim commit, initialises the GraphBLAS submodule, symlinks
   `.claude/CLAUDE.local.md`, renames the cmux tab to `[v] #<N>`.

Its output ends with a `=== next-task: claimed ===` block —
`issue`/`priority`/`category`/`title`/`issue_url`/`branch`/`claim_sha`/
`worktree` — followed by the task's body section from `TASKS.md`. That block
plus the linked GitHub issue is your task context; you do not need to read
`TASKS.md` yourself. Announce the pick (title, priority/category, `#N`) and
move on.

Exit codes: `0` claimed and ready · `7` no claimable open task (report that
the board is clear and stop) · `2` guard/usage failure (surface it to the
user; if a claim was already held the message says so and how to release it) ·
`3`/`4`/`5` passed through from `tasks.sh` (dirty TASKS.md / lock timeout /
non-fast-forward `main` — investigate, don't force).

`.claude/scripts/next-task.sh select` is a read-only dry run of the ranked
candidates if you need to look without claiming.

Export the shared target dir (the output echoes this hint) before building or
pushing, so fresh worktrees reuse the ~700 MB rocksdb artifact:

```bash
export CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/../.horndb-shared-target"
```

## Phase 1 — Epic check

Decide honestly: **can this task be completed and merged in a single coherent
PR?** Whole-SPEC items (e.g. "SPEC-07 SPARQL (…)", "SPEC-02 storage (…)") with
broad parenthetical scope usually cannot. (The bootstrap prints an
`epic_hint=` line when the title says EPIC, but the judgment is yours.)

- **Not an epic** → continue to Phase 2 with the task as-is.
- **Epic** → the claim you already hold is the **parent task line**, which is
  exactly right: it stays `[v]` for the duration of one increment. Break the
  task into **shippable increments** and create one **sub-issue per
  increment**, each with matching `priority:` + `category:` labels (see
  `CLAUDE.md` taxonomy):
  ```bash
  gh issue create --repo sunstoneinstitute/horndb \
    --title "<increment title>" \
    --label "priority: <p>" --label "category: <c>" \
    --body-file <file>
  ```
  Link each sub-issue under the parent `#N` (`gh sub-issue add … || true`; if
  unavailable, a `- [ ] #<child>` task list in the parent body, and reference
  `#N` in each child). Leave a link to `#N` in the `TASKS.md` body for this
  task so the breakdown is discoverable.

  Then **select the first open increment's sub-issue** and treat *that* as the
  unit of work for the rest of this run. After the increment merges, Phase 8
  releases the parent back to `[ ]` (`unclaim`) so the next increment is
  pickable; the parent only flips `[x]` when the **last** increment merges and
  every sub-issue is `CLOSED`. Use the **sub-issue** number on the cmux tab
  and in the PR.

## Phase 2 — Implement

Work from the worktree the bootstrap created:

1. Research the relevant SPEC(s), plan(s), `INTEGRATION-NOTES.md`, and the
   task body the bootstrap printed. Respect each crate's gotchas in `CLAUDE.md`.
2. Use `superpowers:writing-plans` to produce the implementation plan, then
   **`superpowers:subagent-driven-development`** to execute it with atomic
   commits.
3. Honour the harness-first rule (SPEC-00): the task is not satisfied until its
   referenced subset in the harness is green. Grow a subset if needed, never
   bypass it.

## Phase 3 — Verify (real commands, real output)

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

## Phase 4 — Bookkeeping commit (no TASKS.md on the branch)

**Do not touch `TASKS.md` in the worktree branch.** Its `[v]`→`[x]` (or epic
release) transition is a locked commit on `main` in Phase 8, *after* the merge.
The feature branch's closing commit carries only the **non-TASKS.md** docs +
code:

- Flip the matching **Status** field in `docs/architecture.md` (typically
  `planned` → `implemented`).
- Update any crate `STAGE1-ACCEPTANCE.md` / `INTEGRATION-NOTES.md` the work
  touched.

> **Docs-sync note.** `CLAUDE.md` asks for `TASKS.md` and `docs/architecture.md`
> to move in the same commit. Under `/next-task` they intentionally split:
> `architecture.md` rides the PR, the TASKS.md checkbox flip is a separate
> locked commit on `main` (Phase 8). This is the one sanctioned exception; the
> two still converge on `main` within the same task.

Commit message should describe the work; reference the issue.

**Do not close anything yet.** Closure is merge-gated: the issue closes via
`Closes #<N>` on merge, and the TASKS.md flip happens right after (Phase 8). Do
**not** run `gh issue close` or `tasks.sh complete` here.

## Phase 5 — Open the PR

```bash
git push -u origin "<branch>"
```

Open the PR with base `main`. The body must include **`Closes #<N>`** (for an
epic increment, `Closes #<sub-issue>` and reference the parent `#<N>`) so the
merge auto-closes the tracking issue. Prefer the project's `new-pr` skill if
available, otherwise:

```bash
gh pr create --repo sunstoneinstitute/horndb --base main \
  --title "<concise title>" --body-file <pr-body-file>
```

Do **not** advertise the assistant in the commit, PR title, or PR body (global
rule). Do not add `Co-authored-by` trailers.

**Do not stop here.** Opening the PR is the *start* of the review→merge loop,
not the end. Continue straight into Phase 6.

## Phase 6 — Review the PR with codex

Get an independent, cross-model review from **codex** (a different model family
than the one that wrote the code — that independence is the value), **if codex
is available** (`command -v codex`); otherwise see the availability bullet
below. Run from the worktree, on the feature branch, reviewing the branch diff
against `main`:

```bash
cd ".worktrees/<branch>"
codex review --base main 2>&1 | tee /tmp/codex-review-<N>.txt
```

Notes and fallbacks:

- `codex review --base main` reviews everything on the branch vs `main`
  non-interactively with codex's built-in review instructions.
- **`--base` and a custom `[PROMPT]` are mutually exclusive** in this codex
  version. To steer the review with task-specific instructions, drop `--base`
  and pass the prompt as the positional arg (reviews the working tree /
  staged changes), or use `--commit <sha>`. For the branch-vs-`main` diff used
  here, take the default instructions (no prompt).
- **codex is used only if it is available.** If codex is not installed or not
  authenticated (`codex login` required) in the current environment, skip
  Phases 6–7: do a careful self-review of the diff instead, note in the PR and
  in the Phase-9 report that the codex review was skipped (and why), and
  continue to Phase 8. Do not block the merge on codex availability, and do
  not try to install or authenticate it yourself.
- Capture the findings verbatim (the `tee` file) so Phase 7 can work through
  them and Phase 9 can summarise them.

## Phase 7 — Address codex's findings with claude (loop)

Work through codex's review **with engineering rigor, not deference** — use the
`superpowers:receiving-code-review` skill. For each finding:

- **Verify it against the code first.** codex can be wrong or miss context.
  If you disagree, say why (you will record the justification in the report)
  rather than applying a change you believe is incorrect.
- **Fix the real ones in the worktree**, with atomic commits, and **push**.
  For non-trivial fixes, prefer `superpowers:subagent-driven-development`;
  trivial one-liners may be applied directly.
- **Re-run the Phase 3 verification** for anything you touched. Never push a
  fix on red.
- Keep `docs/architecture.md` / the issue in sync if a fix changes scope.

Then **re-run `codex review --base main`** to confirm the findings are resolved
and no new ones were introduced. Loop until codex reports no
actionable/blocking findings — or the only remainder are items you have
explicitly, defensibly declined. Cap the loop at ~3 rounds; if it is not
converging, **stop and surface the disagreement to the user**.

## Phase 8 — Merge, then close

Only once the review is clean (or remaining items are justified declines) **and**
all PR checks/CI are green **and** the branch is mergeable:

1. Confirm state:
   ```bash
   gh pr checks <N> --repo sunstoneinstitute/horndb
   gh pr view  <N> --repo sunstoneinstitute/horndb --json mergeable,mergeStateStatus,statusCheckRollup
   ```
2. **Merge** the PR (default to squash):
   ```bash
   gh pr merge <N> --repo sunstoneinstitute/horndb --squash --delete-branch
   ```
   Invoking `/next-task` *is* the standing authorization to merge once the
   loop above is satisfied. If the merge is blocked (e.g. by an auto-mode
   permission classifier), stop and ask the user to authorize it; do not work
   around the block.
3. **Now close the bookkeeping** (the only place closure happens). From the
   **main worktree**:
   ```bash
   cd "<main-worktree-path>"
   git switch main && git fetch origin && git merge --ff-only origin/main
   ```
   - Verify the `Closes #<N>` auto-closed the issue:
     `gh issue view <N> --json state` → `CLOSED`; if not, `gh issue close <N>`.
   - **Single-PR task** — mark it done (locked):
     ```bash
     .claude/scripts/tasks.sh complete --issue <N> --title "<short title>"
     ```
   - **Epic increment** — do **not** mark the parent done. Release it so the
     next increment is pickable:
     ```bash
     .claude/scripts/tasks.sh unclaim --issue <parent-N> --title "<epic>" \
       --reason "increment #<sub> delivered (PR #<pr>)"
     ```
     When the **last** increment merges and every sub-issue is `CLOSED`, use
     `complete --issue <parent-N>` instead to mark the epic `[x]`.
4. Remove the worktree and prune the merged local branch:
   ```bash
   git worktree remove ".worktrees/<branch>"
   git branch -D "<branch>" 2>/dev/null || true
   ```
5. Mark the cmux tab done (sub-issue number for an epic increment):
   ```bash
   command -v cmux >/dev/null && [ -n "$CMUX_TAB_ID" ] && cmux rename-tab "[x] #<N>"
   ```

## Abandoning a claim (if you must stop after claiming)

If the bootstrap claimed a task but you hit a hard stop — Phase-3 gate stays
red and is out of scope, review won't converge, merge
blocked you can't unblock — **release the claim before stopping** so it does
not become a stale `[v]` blocking the next agent. One command (unclaims,
removes the worktree + branch, renames the cmux tab):

```bash
.claude/scripts/next-task.sh abandon --issue <N> --branch "<branch>" \
  --reason "<one-line why>"
```

Then surface the blocker to the user. Leave any *merged* work intact — only
release if the task did not reach a merged PR.

## Phase 9 — Report

Summarise to the user:

- Which task was picked (and why — priority/order), and its `#N`.
- Whether it was treated as an epic (and the sub-issues created, if so).
- The claim commit on `main`, the worktree path, and the branch.
- Verification results (the real command outcomes).
- The PR URL, the **codex review** outcome (or that codex was unavailable and
  the review was skipped, with a self-review done instead), and what was
  changed in response
  (including any findings you defensibly declined, with the reason).
- The **merge** result and confirmation the issue is closed / task `[x]` on
  `main` (or, for an epic, that the parent stays `[v]` with N increments left).
- Any follow-ups left open (especially the remaining epic increments).
