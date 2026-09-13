---
name: triage-github-issue-for-worklode
description: Triage one GitHub issue (or the Worklode inbox as a whole) into a Worklode task — decide whether it's actionable, what kind (feature/bug/chore/design/review/spike), and what priority. Use when asked to "triage an issue", "clear the Worklode inbox", "should this issue become a task", or when a GitHub issue lands with the `worklode` label.
---

# Triaging a GitHub issue for Worklode

Labeling a GitHub issue `worklode` pulls it into the Worklode inbox
automatically — no manual step needed for that part. This skill is the
judgment call that comes next: for each inbox item, decide whether it
becomes a task, and if so, what kind of task.

## Find what needs triage

```bash
lode inbox list --repo sunstoneinstitute/horndb --state new
```

Each row is one untriaged GitHub issue, printed as `REPO  #  TRIAGE  STATE
TITLE`. The `REPO` and `#` columns are exactly the `<repo>` and `<number>`
arguments every `lode inbox` subcommand below takes — read them off this
listing, you don't look them up anywhere else. Read the issue itself (`gh
issue view <n>`) before deciding — the inbox list gives you the queue, not
the content.

## Decide: actionable, duplicate, or noise

For each item, first rule out the two non-task outcomes:

- **Duplicate of existing work** — an open Worklode task or another open
  issue already covers this. Attach it instead of creating a second task:
  ```bash
  lode inbox link <repo> <number> <existing-task-id>
  ```
- **Not actionable** — not a real bug (can't reproduce, works as intended,
  a support question, a duplicate that was already fixed), or too vague to
  act on without more information from the reporter. Dismiss it, and say
  why in the issue before dismissing (a dismissed inbox item creates no
  task, and the "why" only survives on the GitHub issue itself):
  ```bash
  gh issue comment <number> --body "<reason — ask a clarifying question, or explain why this isn't actionable>"
  lode inbox dismiss <repo> <number>
  ```

Everything else gets promoted into a task.

## Decide: what kind

Worklode's task kinds, and which fits a triaged issue:

| Kind | When an issue is this |
|---|---|
| `bug` | Reproducible wrong behavior with a known or discoverable cause. Goes straight to a fix. |
| `design` | The root cause is unclear, or fixing it correctly requires a decision this repo hasn't made yet (e.g. a genuine behavior/semantics question, not just "which function to edit"). File `design` first, not `bug` — the design task's output is the decision, and a follow-up `bug`/`feature` task (`child_of` the design task) does the actual fix once the decision is made. Don't skip this step for anything non-mechanical: a `bug` task with no clear fix just sits stuck in `in_progress`. |
| `feature` | A new capability, not present today, requested by someone outside this loop. |
| `chore` | Maintenance with no user-visible behavior change (dependency bump, doc fix, dead-code removal, CI tweak). |
| `spike` | The issue asks a question this repo doesn't yet know the answer to (a feasibility check, a comparison, "can we even do X") rather than describing a fix or a feature. The spike's output is a finding, typically feeding a follow-up `design` or `feature` task. |
| `review` | The issue asks for a judgment call on something already written (a proposal, a design doc, a PR) rather than for new work. Rare from external GitHub issues; more common for internal-loop review requests. |

`lode task add`/`lode inbox promote` also accept `decision` and `rally`, but
neither fits a triaged GitHub issue in practice — they belong to internal
Worklode workflow, not incoming bug/feature reports.

If unsure between `bug` and `design`, default to `design` — a fix task that
turns out to need a design decision costs more (an `in_progress` task
stalls, needs `task rework` or `task abandon` and a fresh `design` task) than
a design task that turns out to be quick (it just closes fast).

## Decide: priority and scope

`lode inbox promote --priority` takes **critical** / **high** / **medium** /
**low**. Urgency, not effort — a five-minute fix to a crash is `critical`; a
multi-week feature nobody's blocked on is `low`. Check `docs/architecture.md`
for whether the affected subsystem is `implemented`, `planned`, or
`deferred` — an issue against `deferred` scope is almost never `critical`,
since Stage-2/3 work isn't expected yet.

If the issue clearly belongs to a bigger piece of work already tracked
(check `lode task list` and `docs/architecture.md`'s **Status** table),
promote it as a child of that task rather than a bare top-level one:

```bash
lode inbox promote <repo> <number> \
  --kind bug --priority high \
  --title "<clear, specific title — not the issue's title verbatim if that's vague>" \
  --parent <existing-task-id>   # omit if this stands alone
```

Omit `--parent` for anything that doesn't clearly nest under existing work.
`--applies-to` is unrelated to docs — it's a comma-separated list of
*versions* the issue applies to (e.g. `v1.2,v1.3`), for a project that ships
versioned releases. It rarely applies to a triaged HornDB issue; skip it.

## After promoting

Tell the reporter what happened — comment on the GitHub issue with the new
task id and, for anything not `critical`/`high`, a rough sense of when it'll
be picked up (or that it's queued with no ETA):

```bash
gh issue comment <n> --body "Filed as a Worklode task (<task-id>, kind: <kind>, priority: <priority>). <one line on what happens next>."
```

Close the GitHub issue yourself once the fix lands — once the task reaches
its terminal state (e.g. `merged` or `deployed_prod`):

```bash
gh issue close <n> --comment "Fixed by <task-id>, merged in <PR/commit>."
```
