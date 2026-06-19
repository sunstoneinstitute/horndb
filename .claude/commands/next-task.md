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

## Coordination — TASKS.md is lock-serialized

Several `/next-task` agents run in parallel against the **same main worktree**,
so they race on `TASKS.md` claim/complete lines. **Every** TASKS.md transition —
claim, complete, unclaim, and any free-form edit — must go through
`.claude/scripts/tasks.sh`, which:

- must be run **from the main worktree on `main`** (it refuses linked worktrees
  and other branches);
- holds an `flock` (`$GIT_COMMON_DIR/tasks.lock`) across the whole
  edit → `git add` → `commit` → `push origin main` transaction, so concurrent
  agents are serialized and can never clobber each other or push a half-written
  TASKS.md;
- fast-forwards `main` before mutating, and refuses a `claim` whose task is not
  open (exit 9) — the anti-collision check;
- stamps each claim with `session@host · branch · UTC-timestamp` so orphaned
  work is identifiable;
- pushes TASKS.md-only commits with `--no-verify` (no Rust changed, so the
  clippy/build pre-push hook is skipped — that keeps the lock held for seconds,
  not minutes).

Subcommands: `claim` (prints `claim_sha=<sha>`), `complete`, `unclaim`,
`claims` (list active claims with who/where/when/age), `reap --older-than DUR
[--apply]` (release stale claims), `with-lock --message M -- CMD` (escape hatch
for free-form edits). Run `.claude/scripts/tasks.sh help` for the full contract.

**Orphan detection.** Because each claim records session + host + time, a dead
session's `[v]` is recoverable. In Phase 0, after fetching, list claims and
sweep obvious orphans (sized above your longest real task so a slow-but-live
agent is never reaped):

```bash
.claude/scripts/tasks.sh claims                       # who holds what, and for how long
.claude/scripts/tasks.sh reap --older-than 12h        # dry run: what looks orphaned
# .claude/scripts/tasks.sh reap --older-than 12h --apply   # release them
```

Consequence for this workflow: **TASKS.md never appears in a feature branch.**
The claim is a locked commit on `main` (Phase 3); the `[v]`→`[x]` (or epic
release) is a locked commit on `main` *after* the merge (Phase 11). Keeping
TASKS.md off feature branches is what eliminates the cross-agent merge/rebase
conflicts on the index region.

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
4. Sweep for orphaned claims left by dead sessions (see "Orphan detection"):
   ```bash
   .claude/scripts/tasks.sh claims                  # who holds what, and how long
   .claude/scripts/tasks.sh reap --older-than 12h   # dry run; add --apply to release
   ```
   Reaping is optional but keeps the board honest before you select.

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

  Then **select the first open increment's sub-issue** and treat *that* as the
  unit of work for the rest of this run. You claim the **parent task line** for
  the duration of this one increment: Phase 3 flips it `[ ]` → `[v]`, and
  Phase 11 (after the increment merges) releases it back to `[ ]` via
  `tasks.sh unclaim` so the *next* increment is pickable. The parent only flips
  to `[x]` when the **last** increment merges and every sub-issue is `CLOSED`.
  (Because the parent line is the claim unit, only one increment of a given epic
  is in flight at a time — acceptable, since increments are large.)

## Phase 3 — Claim it on `main` (locked, before branching)

Derive a slug and branch name: `branch="task-<N>-<short-kebab-slug>"`.

**All TASKS.md transitions go through `.claude/scripts/tasks.sh`** (see
"Coordination" above) — never hand-edit + commit TASKS.md in `/next-task`. The
helper holds an `flock` across the edit + `git add`/`commit`/`push origin main`,
fast-forwards `main` first, and **refuses the claim if the task is not open**
(exit 9). That refusal is the anti-collision mechanism: if another agent claimed
the task between your Phase-1 selection and now, you lose the race cleanly.

Run it **from the main worktree, on `main`**. The script stamps the claim with
an identity tag — `session@host · branch · UTC-timestamp` — on the index line so
orphaned claims (dead session / crashed host) are detectable and reapable (see
"Orphan detection" below).

```bash
OUT="$(.claude/scripts/tasks.sh claim \
  --issue <N> \
  --branch "<branch>" \
  --session "<SESSION>" \
  --title "<short title>")"
echo "$OUT"                     # -> claim_sha=<sha>
CLAIM_SHA="${OUT#claim_sha=}"   # the exact commit the worktree must fork from
```

`--session` defaults to `${CLAUDE_CODE_SESSION_ID:0:8}` if omitted; host and
timestamp are filled in by the script. For an epic increment the tag still
identifies the parent claim — note the sub-issue in the cmux tab and PR.

`<N>` is the **TASKS.md task line's** issue number — for an epic increment that
is the **parent** issue (the line carrying the `[ ]`), not the sub-issue. On a
non-zero exit:

- **exit 9** → not open (already claimed/done, or no such line). Return to
  Phase 1 and pick the next open task; do **not** retry the same one.
- **exit 3** → TASKS.md had uncommitted changes (another op mid-flight). Wait a
  moment and retry, or investigate.
- **exit 5** → local `main` diverged from `origin/main`; reconcile, then retry.

Mark the cmux tab claimed (no-op outside cmux). For an epic increment use the
**sub-issue** number on the tab:

```bash
command -v cmux >/dev/null && [ -n "$CMUX_TAB_ID" ] && cmux rename-tab "[v] #<N>"
```

Claiming **before** branching is deliberate: the worktree forks from the exact
claim commit (`$CLAIM_SHA`), so the claim is the worktree's base even if other
agents advance `main` afterwards.

## Phase 4 — Isolated worktree (forked from the claim commit)

Create the worktree under **`.worktrees/`** (not `.claude/worktrees/`),
branching from **`$CLAIM_SHA`** — the commit that carries your claim — **not**
bare `main`, which other agents may have advanced past since you claimed:

```bash
git worktree add ".worktrees/<branch>" -b "<branch>" "$CLAIM_SHA"
```

A fresh worktree needs two one-time fix-ups before it builds:

```bash
# 1. The GraphBLAS submodule is NOT checked out in a new worktree; horndb-closure's
#    build.rs panics without it (missing GraphBLAS_version.cmake).
git -C ".worktrees/<branch>" submodule update --init --recursive \
  crates/closure/vendor/GraphBLAS
# 2. Symlink the gitignored worktree-local instructions (see .claude/CLAUDE.local.md).
mkdir -p ".worktrees/<branch>/.claude"
ln -sfn ../../../.claude/CLAUDE.local.md ".worktrees/<branch>/.claude/CLAUDE.local.md"
```

Then symlink the gitignored worktree-local instructions into it so every
worktree shares the one canonical `.claude/CLAUDE.local.md` (per
`.claude/CLAUDE.local.md` → "Worktree symlinks for this file"):

```bash
mkdir -p ".worktrees/<branch>/.claude"
ln -sfn ../../../.claude/CLAUDE.local.md ".worktrees/<branch>/.claude/CLAUDE.local.md"
```

Work from `.worktrees/<branch>`. To avoid recompiling the ~700 MB rocksdb
artifact in a fresh tree, point Cargo at a shared target dir for this run (per
`CLAUDE.md`) — also export it before any `git push` so the pre-push hook reuses
it instead of recompiling:

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

## Phase 7 — Bookkeeping commit (no TASKS.md on the branch)

**Do not touch `TASKS.md` in the worktree branch.** Its `[v]`→`[x]` (or epic
release) transition is a locked commit on `main` in Phase 11, *after* the merge
— keeping TASKS.md off feature branches is what prevents the cross-agent
conflicts. The feature branch's closing commit carries only the
**non-TASKS.md** docs + code:

- Flip the matching **Status** field in `docs/architecture.md` (typically
  `planned` → `implemented`).
- Update any crate `STAGE1-ACCEPTANCE.md` / `INTEGRATION-NOTES.md` the work
  touched.

> **Docs-sync note.** `CLAUDE.md` asks for `TASKS.md` and `docs/architecture.md`
> to move in the same commit. Under `/next-task` they intentionally split:
> `architecture.md` rides the PR, the TASKS.md checkbox flip is a separate
> locked commit on `main` (Phase 11). This is the one sanctioned exception,
> because TASKS.md is the multi-agent contention point; the two still converge
> on `main` within the same task.

Commit message should describe the work; reference the issue.

**Do not close anything yet.** Closure is merge-gated: the issue closes via
`Closes #<N>` on merge, and the TASKS.md flip happens right after (Phase 11). Do
**not** run `gh issue close` or `tasks.sh complete` here. Nothing is marked done
until the merge actually happens.

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
codex review --base main 2>&1 | tee /tmp/codex-review-<N>.txt
```

Notes and fallbacks:

- `codex review --base main` reviews everything on the branch vs `main`
  non-interactively with codex's built-in review instructions.
- **`--base` and a custom `[PROMPT]` are mutually exclusive** in this codex
  version — `codex review --base main "…instructions…"` errors out. If you want
  to steer the review with task-specific instructions instead of diffing a base
  branch, drop `--base` and pass the prompt as the positional arg (it reviews
  the working tree / staged changes), or use `--commit <sha>`. For the
  branch-vs-`main` diff used here, take the default instructions (no prompt).
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
3. **Now close the bookkeeping** (this is the only place closure happens). The
   `docs/architecture.md` Status change rode the merge; the TASKS.md transition
   is a **separate locked commit on `main`** via the script. From the **main
   worktree**, get onto an up-to-date `main` first:
   ```bash
   cd "<main-worktree-path>"           # where you ran Phases 0–4
   git switch main && git fetch origin && git merge --ff-only origin/main
   ```
   - The `Closes #<N>` in the PR body auto-closes the tracking issue on merge.
     Verify: `gh issue view <N> --repo sunstoneinstitute/horndb --json state`
     → `CLOSED`. If it did not auto-close, `gh issue close <N>`.
   - **Single-PR task** — mark it done (locked):
     ```bash
     .claude/scripts/tasks.sh complete --issue <N> --title "<short title>"
     ```
   - **Epic increment** — do **not** mark the parent done. Release it so the
     next increment is pickable by the next agent (locked), and optionally
     record the delivery in the breakdown note:
     ```bash
     .claude/scripts/tasks.sh unclaim --issue <parent-N> --title "<epic>" \
       --reason "increment #<sub> delivered (PR #<pr>)"
     # optional, still locked — free-form breakdown-note edit:
     # .claude/scripts/tasks.sh with-lock --message "docs(tasks): #<sub> delivered" -- <edit-cmd>
     ```
     When the **last** increment merges and every sub-issue is `CLOSED`, use
     `complete --issue <parent-N>` instead (and `gh issue close <parent>` if it
     did not auto-close) to mark the epic `[x]`.
4. Remove the worktree now that it is merged (and prune the merged local
   branch):
   ```bash
   git worktree remove ".worktrees/<branch>"
   git branch -D "<branch>" 2>/dev/null || true
   ```
5. Mark the cmux tab done — `[x]` mirrors the `[x]` flip that just landed on
   `main`. Use the same `<N>` the tab was claimed with in Phase 3 (the
   sub-issue for an epic increment, even if the parent task stays `[v]`):
   ```bash
   command -v cmux >/dev/null && [ -n "$CMUX_TAB_ID" ] && cmux rename-tab "[x] #<N>"
   ```

## Abandoning a claim (if you must stop after claiming)

If you have already claimed (Phase 3) but then hit a hard stop — Phase-6 gate
stays red and is out of scope, codex unavailable, review won't converge, merge
blocked you can't unblock, or any "cannot proceed safely" — **release the claim
before stopping** so it does not become a stale `[v]` blocking the next agent.
From the main worktree on `main`:

```bash
.claude/scripts/tasks.sh unclaim --issue <N> --title "<short title>" \
  --reason "abandoned: <one-line why>"
git worktree remove ".worktrees/<branch>" --force 2>/dev/null || true
command -v cmux >/dev/null && [ -n "$CMUX_TAB_ID" ] && cmux rename-tab "#<N> (released)"
```

Then surface the blocker to the user. Leave any *merged* work intact — only
release if the task did not reach a merged PR.

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
