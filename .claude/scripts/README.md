# Task-tracking scripts

Three scripts. `next-task.sh` is the deterministic bootstrap for the
`/next-task` workflow; beneath it sit two interchangeable backends for the
task list itself, with the **same subcommand surface** (`claim` / `complete` /
`unclaim` / `claims` / `reap`) so the workflow barely changes between them.

## `next-task.sh` — scripted bootstrap for `/next-task`

Collapses the workflow's preflight → select → claim → worktree phases into one
non-AI command, so an agent running `/next-task` spends its first tool call
getting a claimed task and a ready worktree instead of shepherding a dozen
shell commands (and never has to read `TASKS.md` itself):

```bash
.claude/scripts/next-task.sh select    # read-only: ranked open candidates
.claude/scripts/next-task.sh start     # preflight, orphan report, claim (race-
                                       # retrying), worktree + fixups, context dump
.claude/scripts/next-task.sh abandon --issue N --branch BR --reason R   # bail-out
```

Selection is pure text processing over `TASKS.md`'s `## Index` (open `[ ]`
items, CRITICAL → HIGH → MEDIUM → LOW, then file order; the trailing
`([#N](…))` link identifies the task). Claiming delegates to `tasks.sh`, so
all locking/anti-collision semantics below apply unchanged; a lost race (exit
9) automatically retries the next candidate. Regression tests:
`.claude/scripts/test-next-task.sh` (sandboxed, never touches the real repo).

| | `tasks.sh` (A — **live**) | `tasks-github.sh` (B — **prototype**) |
|---|---|---|
| Source of truth | `TASKS.md` in git | GitHub issues |
| Claim state | `[v]` + tag in `TASKS.md` | `status:in-progress` label + claim comment |
| Mutual exclusion | `flock` (same-host) | comment-id election (any host) |
| Claim identity | `session@host · branch · UTC` tag | same, in a claim comment |
| Where it runs | **main worktree on `main`** only | anywhere (`gh`-only) |
| Merge conflicts | avoided by lock + keeping TASKS.md off branches | impossible (no in-git state) |
| `TASKS.md` | hand-maintained source | generated read-only by `render` |

Both record **who / where / when** for every claim, so orphaned work (dead
session / crashed host) is detectable via `claims` and reapable via
`reap --older-than DUR [--apply]`.

## A — `tasks.sh` (file-based, in use)

The `/next-task` workflow uses this today. Every `TASKS.md` transition is an
`flock`-guarded `edit → add → commit → push origin main` run from the main
worktree. `claim` refuses a non-open task (exit 9 = anti-collision) and prints
`claim_sha=<sha>` for the worktree to fork from. See the script header for the
full contract.

## B — `tasks-github.sh` (Option-B prototype, for evaluation)

Moves task *state* onto the GitHub issue, so there is no volatile claim state in
git and therefore no lock and no merge conflicts:

- **priority / category** → existing `priority: …` / `category: …` labels.
- **claimed / in review / done** → `status:in-progress` / `status:in-review`
  labels / issue closed (via the PR's `Closes #N`).
- **who/where/when** → a structured claim *comment*
  (`<!-- tasks-claim v1 --> … session=… host=… branch=… at=…`).
- **`claim` election:** all agents authenticate as the same GitHub user, so
  assignee can't arbitrate. The claimant posts a marker comment, then the lowest
  GitHub **comment id** among active marker comments wins (ids are globally
  monotonic → a total order, correct across hosts, no local lock). The election
  reads the issue's own comments, which are immediately consistent.
- **`render`** regenerates a read-only `TASKS.md` from `gh issue list`, grouped
  by priority, marking `🔒` claimed / `👀` in review. Epic increments are their
  own issues, so each is claimable independently — no parent-line bottleneck.

Evaluate it (read-only, safe):

```bash
.claude/scripts/tasks-github.sh render --out -     # preview generated list
.claude/scripts/tasks-github.sh claims             # active claims + age
.claude/scripts/tasks-github.sh reap --older-than 12h   # dry run
```

`claim` / `unclaim` / `complete` / `reap --apply` mutate the real issue tracker
(label + comment); try them against a throwaway test issue first.

### Known caveats (prototype)

- **List lag.** GitHub's list-by-label (search *and* REST) takes a few seconds
  to reflect a *just*-added label, so `claims` may omit an issue claimed seconds
  ago. Irrelevant to orphan detection (orphans are old claims) and to the
  election (which reads comments, not the list).
- **Election window.** Two truly simultaneous claimants rely on each other's
  marker comment being visible before the election; GitHub is read-your-writes
  per issue, but a sub-second cross-agent window exists. The `flock` from A could
  be layered on for same-host belt-and-suspenders.
- **Lost prose.** `render` derives the list from issue titles/labels; epic
  breakdown prose lives on the issues, not in `TASKS.md`. That is the
  maintainability trade — less narrative in-repo, single source on GitHub.

## Adopting B

If chosen: rewire `next-task.sh`'s claim path and `/next-task`'s close phase to
call `tasks-github.sh`, run
`render` once to replace `TASKS.md` with the generated view (add a CI check or
hook that re-renders), and retire the `[v]` markers. Until then, A stays the
source of truth and B is evaluation-only.
