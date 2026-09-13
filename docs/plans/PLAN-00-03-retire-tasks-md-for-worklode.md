---
status: draft
date: 2026-09-13
scope: "Remove TASKS.md and its tooling; point every doc/CLAUDE.md sync rule at Worklode; add a GitHub-issue-to-Worklode triage skill"
---

# Retire TASKS.md for Worklode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete `TASKS.md` and its dead tooling, repoint every doc/CLAUDE.md
reference to how outstanding work is tracked so it names Worklode instead,
and add a skill that triages an incoming GitHub issue into a Worklode task.

**Architecture:** This is a documentation/config sweep, not a code change.
`TASKS.md` was superseded by Worklode back in commit `0454cf57` ("Clean up
claude commands (worklode supersedes them)"), which deleted the `/next-task`
and `/start-loop` slash commands but left `TASKS.md` itself, its
`.claude/scripts/*task*.sh` tooling, and dozens of doc cross-references
in place. This plan finishes that migration: delete the file and its dead
scripts, rewrite the *procedural* sync rules (CLAUDE.md's "Keep the docs in
sync", `docs/architecture.md`'s "Keeping this document honest", and the
equivalent paragraphs in `docs/index.md`, `docs/AGENTS.md`,
`docs/adr/README.md`, `docs/plans/AGENTS.md`, `docs/architecture/simd.md`,
`docs/benchmarks.md`, and `.claude/commands/release.md`) to name Worklode
(`lode task`, `lode board`, `lode doc todo`) instead, and strip now-dangling
inline `TASKS.md` citations from the body of `docs/architecture.md` and the
handful of specs that still had forward-looking "sync TASKS.md" checklist
items. GitHub issues remain the external intake channel — label one
`worklode` and it lands in the Worklode inbox (`lode inbox list`), where the
new skill (`triage-github-issue-for-worklode`) helps decide whether to
promote it into a task, and with what kind/priority.

**Explicitly out of scope (do not touch):** `docs/plans/PLAN-*.md` files —
these are historical implementation logs (`docs/plans/AGENTS.md`: "commit-
message-grade context, not a source of truth for current behaviour") and
retroactively editing them to scrub old `TASKS.md` citations would rewrite
history for no benefit. Also out of scope: historical `TASKS.md #N`
citations inside specs whose own `status:` is `implemented` for the row in
question (the work is done; the citation is a record of how it was tracked
at the time, not a live instruction) — see Task 5 for the exact list.
Source-code comments under `crates/**` citing old `TASKS.md #N` issue
numbers are also out of scope — the user's ask was CLAUDE.md, `.claude/**`,
and `docs/**` only.

**Tech Stack:** Markdown, bash (script deletion only), the `lode` CLI
(Worklode) command surface documented in the `worklode` skill.

**Spec:** None — this executes a direct user request. The investigation
behind it (which files reference `TASKS.md`, and whether each reference is
a live procedure or historical trivia) was done inline in this session; the
per-file line numbers and exact replacement text below are that
investigation's output, not something the executor needs to re-derive.

## Global Constraints

- No Rust code changes. `cargo fmt`/`clippy`/`build` gates do not apply to
  this plan; each task's "test" is a `grep` verification, not a compiler run.
- Every edit must be an exact string match against the current file content
  — re-read the file immediately before editing if a previous task in this
  plan touched a file this task also touches, since line numbers shift.
- Do not invent Worklode task ids (`WL-N`) or GitHub issue numbers. Where an
  old citation named a specific `TASKS.md` line item with no durable
  replacement id available, drop the dangling clause rather than fabricate
  one (e.g. "`in TASKS.md`" is deleted, not replaced with a guessed `WL-N`).
- Existing GitHub issue links (`[#N](https://github.com/sunstoneinstitute/horndb/issues/N)`)
  that already exist in a sentence stay untouched — only the trailing
  "`... in TASKS.md`" clause tacked onto them is removed.
- Commit after each task (this plan's tasks are independent files/clusters;
  don't batch them into one commit).

---

### Task 1: Delete TASKS.md and its dead tooling

**Files:**
- Delete: `TASKS.md`
- Delete: `.claude/scripts/tasks.sh`
- Delete: `.claude/scripts/next-task.sh`
- Delete: `.claude/scripts/tasks-github.sh`
- Delete: `.claude/scripts/test-tasks.sh`
- Delete: `.claude/scripts/test-next-task.sh`
- Delete: `.claude/scripts/README.md` (its entire content documents these five scripts)
- Modify: `.claude/commands/release.md:38`

**Context:** These scripts backed the `/next-task` slash command, which was
already deleted in commit `0454cf57` ("Clean up claude commands (worklode
supersedes them)"). They have had no caller since. Confirm before deleting:

- [ ] **Step 1: Confirm no other script or hook calls these files**

Run:
```bash
grep -rn "tasks\.sh\|next-task\.sh\|tasks-github\.sh" .claude/ --include=*.json --include=*.md --include=*.sh | grep -v "^\.claude/scripts/"
```
Expected: no output (nothing outside `.claude/scripts/` itself references
them). If this prints a hit, stop and report it instead of deleting — do not
delete a script something else still calls.

- [ ] **Step 2: Delete the file and the six scripts**

```bash
git rm TASKS.md .claude/scripts/tasks.sh .claude/scripts/next-task.sh \
  .claude/scripts/tasks-github.sh .claude/scripts/test-tasks.sh \
  .claude/scripts/test-next-task.sh .claude/scripts/README.md
```

- [ ] **Step 3: Fix the dangling reference in `.claude/commands/release.md`**

Current line 38:
```
If everything is current, say so and move on. Do not treat internal-only docs (`TASKS.md`, plans, INTEGRATION-NOTES) as release blockers — they track outstanding work, not the release surface.
```
Replace with:
```
If everything is current, say so and move on. Do not treat internal-only docs (plans, INTEGRATION-NOTES) or open Worklode tasks as release blockers — they track outstanding work, not the release surface.
```

- [ ] **Step 4: Verify**

Run: `git status --short . ; ls .claude/scripts/`
Expected: `TASKS.md` gone, `.claude/scripts/` contains only
`cargo-sweep-hook.sh` and `release.py` (and their untouched test/README
files if any remain — there are none for these two).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Remove TASKS.md and its dead /next-task tooling (superseded by Worklode)"
```

---

### Task 2: Rewrite root CLAUDE.md to point at Worklode

**Files:**
- Modify: `CLAUDE.md` (repo root)

**Interfaces:**
- Produces: the canonical wording other tasks in this plan reuse verbatim:
  "Worklode" as the outstanding-work source of truth, `lode task`/`lode
  board`/`lode doc todo` as the commands named, and "label an issue
  `worklode` to pull it into the Worklode inbox" as the GitHub-intake
  description. Tasks 3-6 should match this phrasing, not invent their own.

- [ ] **Step 1: Read the current file**

Read `CLAUDE.md` in full before editing — the exact text below must match
byte-for-byte or the edit tool will fail.

- [ ] **Step 2: Remove the TASKS.md bullet from "Authoritative documents" and add a Worklode pointer**

Find:
```
- `TASKS.md` — Stage-1 follow-ups. Ordered CRITICAL → HIGH → MEDIUM → LOW. When picking up a task, move it to its own commit and check it off in the same commit. You can push commits that only contain task claims/updates to origin without asking. Its header carries the task↔GitHub-issue mirroring procedure.
```
Delete that line entirely (remove the whole bullet, including its leading
`- ` and trailing newline so the surrounding list stays well-formed).

Then, immediately after the "Authoritative documents" bulleted list (before
the "### Where specs and plans live" heading), insert a new subsection:

```markdown
### Work tracking

This project is tracked in Worklode. Work is claimed, not assigned — load
the `worklode` skill before filing or finding a task, and before creating or
reading a spec, ADR, or plan. `lode board` and `lode task list` show open
work; there is no in-repo task file anymore.

GitHub issues remain the intake channel for feature requests and bug
reports from outside the loop. Label an issue `worklode` and it lands in the
Worklode inbox (`lode inbox list`); use the `triage-github-issue-for-worklode`
skill to decide whether to promote it into a task and with what kind/priority.
```

- [ ] **Step 3: Rewrite "Keep the docs in sync"**

Find the full section:
```
### Keep the docs in sync (do this in the same commit)

`docs/architecture.md`, `TASKS.md`, and the SPECs/plans are linked views of the same reality. When you edit one, update the others so they never drift:

- **Change `TASKS.md`** (check off, add, remove, re-scope) → update the matching **Status** field in `docs/architecture.md`. Checking off a task usually flips a row **planned** → **implemented**; adding one usually flips **specified** → **planned**. Mirror the change to the task's GitHub issue too — procedure in the `TASKS.md` header.
- **Change a SPEC or plan** such that the outstanding work changes → update `TASKS.md` (add or re-scope the tracking task), then reflect the new state in `docs/architecture.md`.
- **Add, remove, rename, or re-type a metric or a metric label** (in `crates/metrics/`, or change an emit site / label-value enum in `crates/metrics/src/labels.rs`) → update the matching row in `docs/metrics.md` in the **same commit**. This covers the scraped name, type, labels, units/buckets, and meaning. `crates/metrics/src/*.rs` is the source of truth; if `docs/metrics.md` disagrees, fix the doc.

**Feature-branch exception (sanctioned):** `TASKS.md` is lock-serialized on `main` via `.claude/scripts/tasks.sh` and never appears on a feature branch. A feature-branch PR carries the `docs/architecture.md` (and other docs) updates; the matching `TASKS.md` transition lands as a locked commit on `main` immediately after the merge. A PR that updates `architecture.md` without touching `TASKS.md` is therefore correct, not a sync violation — reviewers should not flag it. The two views converge on `main` within the same task.

Source of truth: SPECs for *intent*, `TASKS.md` for *outstanding work*, `docs/architecture.md` for *current state*, `docs/metrics.md` for *the metrics surface*. When they disagree, **the code wins** — fix whichever is stale.

**Never write a `#N` issue reference you haven't verified** (`gh issue view N`). A bare `#N` used as informal shorthand for a topic — rather than the real issue number — propagates across `TASKS.md`, `docs/benchmarks.md`, `docs/architecture.md`, and the SPECs, and unwinding it later is a multi-file `sed` sweep. If the issue isn't filed yet, write `#TODO` (or file it first), not a placeholder number.
```

Replace the whole section with:
```
### Keep the docs in sync (do this in the same commit)

`docs/architecture.md` and the SPECs/plans are linked views of the same
reality; Worklode tracks the outstanding work that connects them. When you
edit one, update the others so they never drift:

- **File or re-scope a Worklode task** (`lode task add`, `lode task show`)
  such that a subsystem's status changes → update the matching **Status**
  field in `docs/architecture.md`. Closing a task usually flips a row
  **planned** → **implemented**; filing one usually flips **specified** →
  **planned**.
- **Change a SPEC or plan** such that the outstanding work changes → file or
  re-scope the matching Worklode task, then reflect the new state in
  `docs/architecture.md`.
- **Add, remove, rename, or re-type a metric or a metric label** (in `crates/metrics/`, or change an emit site / label-value enum in `crates/metrics/src/labels.rs`) → update the matching row in `docs/metrics.md` in the **same commit**. This covers the scraped name, type, labels, units/buckets, and meaning. `crates/metrics/src/*.rs` is the source of truth; if `docs/metrics.md` disagrees, fix the doc.

Source of truth: SPECs for *intent*, Worklode for *outstanding work*,
`docs/architecture.md` for *current state*, `docs/metrics.md` for *the
metrics surface*. When they disagree, **the code wins** — fix whichever is
stale.

**Never write a `#N` or `WL-N` reference you haven't verified** (`gh issue
view N` for a GitHub issue, `lode task show WL-N` for a Worklode task). A
bare id used as informal shorthand for a topic — rather than the real one —
propagates across `docs/benchmarks.md`, `docs/architecture.md`, and the
SPECs, and unwinding it later is a multi-file `sed` sweep. If the work isn't
filed yet, write `#TODO` (or file it first), not a placeholder id.
```

- [ ] **Step 4: Verify**

Run: `grep -n "TASKS\.md" CLAUDE.md`
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "Point CLAUDE.md's doc-sync rule at Worklode instead of TASKS.md"
```

---

### Task 3: Rewrite docs/architecture.md

**Files:**
- Modify: `docs/architecture.md`

**Context:** `docs/architecture.md` has ~20 body citations of the form
"`... in TASKS.md`" or "`(TASKS.md #N)`" scattered through its status
table, plus a header paragraph, a maintenance callout, and a full
"Keeping this document honest" closing section. Re-read the file before
starting — line numbers below are from the investigation pass and will have
shifted if anything upstream in the file changed since.

- [ ] **Step 1: Read the file**

- [ ] **Step 2: Fix the header (originally lines 5-11)**

Find:
```
This document is the single-page map of HornDB's architecture: what each
subsystem is, how the pieces fit together, and — for every part — what
state it is actually in. It is synthesised from the authoritative SPECs
(`docs/specs/SPEC-00..10-*.md`) and their Stage-1 implementation plans
(`docs/plans/2026-05-24-*.md`).

For the canonical "why" read `docs/specs/SPEC-00-vision.md` first; for the
ground-truth gap list read `TASKS.md`. This document sits between them: the
SPECs say what *should* exist, `TASKS.md` tracks the work to close the gaps,
and the **Status** fields here say what exists *today*.
```
Replace with:
```
This document is the single-page map of HornDB's architecture: what each
subsystem is, how the pieces fit together, and — for every part — what
state it is actually in. It is synthesised from the authoritative SPECs
(`docs/specs/SPEC-NN-*.md`) and their implementation plans
(`docs/plans/PLAN-NN-MM-*.md`).

For the canonical "why" read `docs/specs/SPEC-00-vision.md` first; for the
ground-truth gap list, run `lode board` or `lode task list`. This document
sits between them: the SPECs say what *should* exist, Worklode tracks the
work to close the gaps, and the **Status** fields here say what exists
*today*.
```
(This also fixes the stale `SPEC-00..10`/`2026-05-24` naming noted in the
earlier docs-sync check — specs now run past SPEC-10 and plans use
`PLAN-NN-MM` naming, not date prefixes.)

- [ ] **Step 3: Fix the Status table's "planned" row and the Maintenance callout**

Find:
```
| **planned** | A concrete follow-up exists in `TASKS.md` to build or finish it. |
```
Replace with:
```
| **planned** | A concrete follow-up exists in Worklode to build or finish it. |
```

Find:
```
> **Maintenance:** the Status fields here and the checkboxes in `TASKS.md`
> are two views of the same reality and must be kept in sync. See
> [Keeping this document honest](#keeping-this-document-honest) and the rule
> in the root `CLAUDE.md`.
```
Replace with:
```
> **Maintenance:** the Status fields here and Worklode's task state are two
> views of the same reality and must be kept in sync. See
> [Keeping this document honest](#keeping-this-document-honest) and the rule
> in the root `CLAUDE.md`.
```

- [ ] **Step 4: Strip dangling inline citations from the status table body**

Apply each of these exact replacements (grep for the old text first if a
line number has shifted):

1. Find `leaf task [#207](https://github.com/sunstoneinstitute/horndb/issues/207) in \`TASKS.md\`).` — replace with `leaf task [#207](https://github.com/sunstoneinstitute/horndb/issues/207)).`

2. Find `phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217) in \`TASKS.md\`.` — replace with `phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217).`

3. Find `decomposed into \`TASKS.md\` phase tasks` — replace with `decomposed into phase tasks`

4. Find `([#228](https://github.com/sunstoneinstitute/horndb/issues/228) in \`TASKS.md\`): export/import cover` — replace with `([#228](https://github.com/sunstoneinstitute/horndb/issues/228)): export/import cover`

5. Find `\`SPEC-25\` S5 ([#229](https://github.com/sunstoneinstitute/horndb/issues/229) in \`TASKS.md\`). Done:` — replace with `\`SPEC-25\` S5 ([#229](https://github.com/sunstoneinstitute/horndb/issues/229)). Done:`

6. Find `\`SPEC-25\` S1 ([#225](https://github.com/sunstoneinstitute/horndb/issues/225) in \`TASKS.md\`, \`PLAN-25-01\`): begin/end` — replace with `\`SPEC-25\` S1 ([#225](https://github.com/sunstoneinstitute/horndb/issues/225), \`PLAN-25-01\`): begin/end`

7. Find `the naïve oracle. \`TASKS.md\` #2. Downstream F5` — replace with `the naïve oracle. Downstream F5`

8. Find `tracked in [#57]). \`TASKS.md\` #48. |` — replace with `tracked in [#57]). |`

9. Find `generator (Stage-2, native-linkage heavy) — see \`TASKS.md\` / SPEC-08.` — replace with `generator (Stage-2, native-linkage heavy) — see SPEC-08 / Worklode.`

10. Find `Open increment under \`TASKS.md\` MEDIUM · *Completeness* — "SPEC-08 ML" (#8).` — replace with `Open increment — "SPEC-08 ML" (#8).`

11. Find `epic in \`TASKS.md\` (#9), split into shippable increments.` — replace with `epic (#9), split into shippable increments.`

12. Find `Tracked as a HIGH *Completeness* task in \`TASKS.md\`.` — replace with `Tracked as a HIGH *Completeness* task in Worklode.`

13. Find `Tracked as a\nHIGH *Performance* task in \`TASKS.md\`.` — replace with `Tracked as a\nHIGH *Performance* task in Worklode.`

14. Find `stay planned in \`TASKS.md\`; reasoning-strategy selection stays out of the optimizer until phase 6.**` — replace with `stay planned in Worklode; reasoning-strategy selection stays out of the optimizer until phase 6.**`

15. Find `(compressed side-table, on-demand re-derivation) is **planned**\n(\`TASKS.md\` SPEC-04 rules).` — replace with `(compressed side-table, on-demand re-derivation) is **planned**\n(Worklode, SPEC-04 rules).`

16. Find `remain **deferred** (\`TASKS.md\`, RDF 1.2 entries — both \`[x]\`). The OWL 2 RL` — replace with `remain **deferred** (RDF 1.2 entries tracked in Worklode). The OWL 2 RL`

17. Find `\`docs/benchmarks.md\` rows in sync with the \`TASKS.md\` performance entries.` — replace with `\`docs/benchmarks.md\` rows in sync with Worklode's performance-tracking tasks.`

18. Find `**implemented** (with open gaps tracked in \`TASKS.md\`) |` — replace with `**implemented** (with open gaps tracked in Worklode) |`

19. Find `remaining phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214), [#216](https://github.com/sunstoneinstitute/horndb/issues/216), [#217](https://github.com/sunstoneinstitute/horndb/issues/217) in \`TASKS.md\` | **high** | [#186](https://github.com/sunstoneinstitute/horndb/issues/186) |` — replace with `remaining phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214), [#216](https://github.com/sunstoneinstitute/horndb/issues/216), [#217](https://github.com/sunstoneinstitute/horndb/issues/217) | **high** | [#186](https://github.com/sunstoneinstitute/horndb/issues/186) |`

- [ ] **Step 5: Rewrite "Keeping this document honest"**

Find:
```
## Keeping this document honest

The Status fields above mirror the checkbox state in `TASKS.md`. They drift
apart the moment one is edited without the other. Two rules (also recorded in
the root `CLAUDE.md`):

1. **When you change `TASKS.md`** (check off, add, remove, or re-scope a task),
   update the matching **Status** field here in the same commit — e.g.
   checking off "SPEC-07 DESCRIBE" flips that row from **planned** to
   **implemented**.
2. **When you change a SPEC or plan** (`docs/specs/` or `docs/plans/`) such
   that the work-to-do changes, update `TASKS.md` in the same commit — add or
   re-scope the tracking task — and then reflect it here.

Source of truth for *intent* is the SPECs; for *outstanding work* it is
`TASKS.md`; for *current state* it is this document. When they disagree, the
code wins — fix whichever is stale.
```
Replace with:
```
## Keeping this document honest

The Status fields above mirror Worklode's task state. They drift apart the
moment one is edited without the other. Two rules (also recorded in the
root `CLAUDE.md`):

1. **When you change a Worklode task** (file, close, or re-scope one),
   update the matching **Status** field here in the same commit — e.g.
   closing "SPEC-07 DESCRIBE" flips that row from **planned** to
   **implemented**.
2. **When you change a SPEC or plan** (`docs/specs/` or `docs/plans/`) such
   that the work-to-do changes, file or re-scope the matching Worklode task
   in the same commit — and then reflect it here.

Source of truth for *intent* is the SPECs; for *outstanding work* it is
Worklode; for *current state* it is this document. When they disagree, the
code wins — fix whichever is stale.
```

- [ ] **Step 6: Verify**

Run: `grep -n "TASKS\.md" docs/architecture.md`
Expected: no output.

- [ ] **Step 7: Commit**

```bash
git add docs/architecture.md
git commit -m "Point docs/architecture.md's sync rule and body citations at Worklode"
```

---

### Task 4: Rewrite the remaining docs/** sync-rule references

**Files:**
- Modify: `docs/benchmarks.md` (4 sites)
- Modify: `docs/index.md` (3 sites)
- Modify: `docs/AGENTS.md` (1 site)
- Modify: `docs/adr/README.md` (2 sites)
- Modify: `docs/plans/AGENTS.md` (1 site)
- Modify: `docs/architecture/simd.md` (1 site)

- [ ] **Step 1: docs/benchmarks.md**

Find:
```
Live gaps are
tracked in [`../TASKS.md`](../TASKS.md).
```
Replace with:
```
Live gaps are
tracked in Worklode (`lode board`).
```

Find:
```
the full corpus expansion is Stage-2
  work (`../TASKS.md` MEDIUM).
```
Replace with:
```
the full corpus expansion is Stage-2
  work (tracked in Worklode, MEDIUM).
```

Find:
```
record both means **and** the criterion HTML
reports (under `target/criterion/`) for any number quoted in `TASKS.md`, a
commit message, or a published artefact.
```
Replace with:
```
record both means **and** the criterion HTML
reports (under `target/criterion/`) for any number quoted in a Worklode
task, a commit message, or a published artefact.
```

Find:
```
When a bench moves into *Measured* (or moves between RED and GREEN), update
the relevant row, link the issue or plan that closed the gap, and update the
corresponding entry in `../TASKS.md` and the Status field in
`architecture.md` in the same commit.
```
Replace with:
```
When a bench moves into *Measured* (or moves between RED and GREEN), update
the relevant row, link the issue or plan that closed the gap, and update the
corresponding Worklode task and the Status field in
`architecture.md` in the same commit.
```

- [ ] **Step 2: docs/index.md**

Find:
```
- [`../TASKS.md`](../TASKS.md) — live follow-up list and current gaps.
```
Replace with:
```
- Outstanding work lives in Worklode (`lode board`, `lode task list`) — there is no in-repo task file.
```

Find:
```
This is the detailed, kept-current status record — read it, not this index, for plan history, issue numbers, and bench numbers. Kept in sync with `../TASKS.md`.
```
Replace with:
```
This is the detailed, kept-current status record — read it, not this index, for plan history, issue numbers, and bench numbers. Kept in sync with Worklode.
```

Find:
```
Each entry names the spec (and crate notes) to read; current implementation status, plan history, issue links, and bench numbers live in `architecture.md` (section noted) or the `TASKS.md`/epics table — not here.
```
Replace with:
```
Each entry names the spec (and crate notes) to read; current implementation status, plan history, issue links, and bench numbers live in `architecture.md` (section noted) or Worklode — not here.
```

- [ ] **Step 3: docs/AGENTS.md**

Find:
```
- `docs/architecture.md` is the single-page **status map**: one row per subsystem/feature with an implemented / specified / planned / deferred **Status**, kept in sync with `../TASKS.md`. It says *what exists today*, briefly.
```
Replace with:
```
- `docs/architecture.md` is the single-page **status map**: one row per subsystem/feature with an implemented / specified / planned / deferred **Status**, kept in sync with Worklode. It says *what exists today*, briefly.
```

- [ ] **Step 4: docs/adr/README.md**

Find:
```
- **`../../TASKS.md`** — the *outstanding work* to close the gaps.
```
Replace with:
```
- **Worklode** (`lode board`, `lode task list`) — the *outstanding work* to close the gaps.
```

Find:
```
4. If it changes outstanding work or current state, update `../../TASKS.md` and `../architecture.md` in the same commit (the docs-sync rule in the root `CLAUDE.md`).
```
Replace with:
```
4. If it changes outstanding work or current state, file/update the matching Worklode task and `../architecture.md` in the same commit (the docs-sync rule in the root `CLAUDE.md`).
```

- [ ] **Step 5: docs/plans/AGENTS.md**

Find:
```
- When a plan changes the outstanding work, update `TASKS.md` and
  `../architecture.md` in the same commit (sync rules in the root `AGENTS.md`).
```
Replace with:
```
- When a plan changes the outstanding work, file/update the matching
  Worklode task and `../architecture.md` in the same commit (sync rules in
  the root `AGENTS.md`).
```

- [ ] **Step 6: docs/architecture/simd.md**

Find:
```
then sync `docs/benchmarks.md`,
`docs/architecture.md`, and `TASKS.md`.
```
Replace with:
```
then sync `docs/benchmarks.md`,
`docs/architecture.md`, and the corresponding Worklode task.
```

- [ ] **Step 7: Verify**

Run:
```bash
grep -n "TASKS\.md" docs/benchmarks.md docs/index.md docs/AGENTS.md docs/adr/README.md docs/plans/AGENTS.md docs/architecture/simd.md
```
Expected: no output.

- [ ] **Step 8: Commit**

```bash
git add docs/benchmarks.md docs/index.md docs/AGENTS.md docs/adr/README.md docs/plans/AGENTS.md docs/architecture/simd.md
git commit -m "Point remaining docs/** sync-rule references at Worklode"
```

---

### Task 5: Rewrite the living specs' forward-looking TASKS.md references

**Files:**
- Modify: `docs/specs/SPEC-00-vision.md`
- Modify: `docs/specs/SPEC-07-sparql-frontend.md`
- Modify: `docs/specs/SPEC-13-shared-graphblas-build.md`
- Modify: `docs/specs/SPEC-14-lubm-rdfox-comparison.md`
- Modify: `docs/specs/SPEC-17-metrics.md`
- Modify: `docs/specs/SPEC-19-streaming-runtime-pushdown.md`
- Modify: `docs/specs/SPEC-28-named-graph-dataset-semantics.md`
- **Do not touch:** `docs/specs/SPEC-05-closure-backend.md` ("Fork A
  implemented (TASKS.md #12)" — historical, Fork A is done),
  `docs/specs/SPEC-16-id-based-slot-rows.md`,
  `docs/specs/SPEC-20-join-probe-streaming.md`,
  `docs/specs/SPEC-22-http-streaming-results.md` (all three are
  `status: implemented` specs whose `TASKS.md` mention documents a
  completed item's tracking history, not a live instruction).

- [ ] **Step 1: SPEC-00-vision.md**

Find:
```
lifting that to real support is the Stage-2 migration tracked in `TASKS.md`.
```
Replace with:
```
lifting that to real support is the Stage-2 migration tracked in Worklode.
```

- [ ] **Step 2: SPEC-07-sparql-frontend.md**

Find:
```
- RDF 1.2 triple terms and the corresponding SPARQL surface — Stage 2 priority (tracked in `TASKS.md`). We follow W3C RDF 1.2, not the community RDF-star extension it superseded.
```
Replace with:
```
- RDF 1.2 triple terms and the corresponding SPARQL surface — Stage 2 priority (tracked in Worklode). We follow W3C RDF 1.2, not the community RDF-star extension it superseded.
```

- [ ] **Step 3: SPEC-13-shared-graphblas-build.md**

Find:
```
- `TASKS.md` — check for the LOW "disk pressure during parallel worktree runs"
  operational item; cross-reference or update it (and its mirrored GitHub issue
  per the repo's sync rule) if present.
```
Replace with:
```
- Worklode — check `lode task list` for the LOW "disk pressure during
  parallel worktree runs" operational item; cross-reference or update it if
  present.
```

- [ ] **Step 4: SPEC-14-lubm-rdfox-comparison.md**

Find:
```
**Tracks:** TASKS.md MEDIUM · _Conformance_ — SPEC-01 harness (RDFox A/B) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
```
Replace with:
```
**Tracks:** Worklode MEDIUM · _Conformance_ — SPEC-01 harness (RDFox A/B) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
```

Find:
```
5. `docs/benchmarks.md` Stage-1 row updated **status-only** (no RDFox number);
   `TASKS.md` #10 reflects the new state; pre-push gate (clippy + build) stays
   green.
```
Replace with:
```
5. `docs/benchmarks.md` Stage-1 row updated **status-only** (no RDFox number);
   the corresponding Worklode task reflects the new state; pre-push gate
   (clippy + build) stays green.
```

- [ ] **Step 5: SPEC-17-metrics.md**

Find:
```
8. `docs/architecture.md` and `TASKS.md` updated; GitHub tracking issue mirrored.
```
Replace with:
```
8. `docs/architecture.md` updated; the corresponding Worklode task tracks remaining work.
```

Find:
```
- `TASKS.md`: add the metrics epic + slice-1 and fan-out tasks; mirror to a GitHub issue
  per the TASKS.md header procedure.
```
Replace with:
```
- Worklode: file the metrics epic + slice-1 and fan-out tasks (`lode task add`).
```

- [ ] **Step 6: SPEC-19-streaming-runtime-pushdown.md**

Find:
```
- **Benchmark:** record the `agg_profile` deltas and the hornbench SPB-256
  aggregation-qps move in `docs/benchmarks.md`; sync `TASKS.md` and `docs/architecture.md`
  in the same commit (per root `CLAUDE.md` doc-sync rule).
```
Replace with:
```
- **Benchmark:** record the `agg_profile` deltas and the hornbench SPB-256
  aggregation-qps move in `docs/benchmarks.md`; sync the corresponding
  Worklode task and `docs/architecture.md` in the same commit (per root
  `CLAUDE.md` doc-sync rule).
```

Find:
```
7. Benchmark on hornbench + docs sync (`docs/benchmarks.md`, `TASKS.md`,
   `docs/architecture.md`).
```
Replace with:
```
7. Benchmark on hornbench + docs sync (`docs/benchmarks.md`, Worklode,
   `docs/architecture.md`).
```

- [ ] **Step 7: SPEC-28-named-graph-dataset-semantics.md**

Find:
```
8. **Docs stay in sync (in-commit).** `docs/architecture.md` (including the
   stale `:319` claim that the store is default-graph-only), `TASKS.md`,
   `docs/specs/README.md`, and `docs/index.md` are updated in the commits that
   introduce the corresponding behaviour, per the root sync rules.
```
Replace with:
```
8. **Docs stay in sync (in-commit).** `docs/architecture.md` (including the
   stale `:319` claim that the store is default-graph-only), the
   corresponding Worklode task, `docs/specs/README.md`, and `docs/index.md`
   are updated in the commits that introduce the corresponding behaviour,
   per the root sync rules.
```

- [ ] **Step 8: Verify**

Run:
```bash
grep -n "TASKS\.md" docs/specs/SPEC-00-vision.md docs/specs/SPEC-07-sparql-frontend.md \
  docs/specs/SPEC-13-shared-graphblas-build.md docs/specs/SPEC-14-lubm-rdfox-comparison.md \
  docs/specs/SPEC-17-metrics.md docs/specs/SPEC-19-streaming-runtime-pushdown.md \
  docs/specs/SPEC-28-named-graph-dataset-semantics.md
```
Expected: no output. (The four specs listed as "do not touch" in this
task's header will still show `TASKS.md` hits if grepped — that's correct
and expected; don't grep those.)

- [ ] **Step 9: Commit**

```bash
git add docs/specs/SPEC-00-vision.md docs/specs/SPEC-07-sparql-frontend.md \
  docs/specs/SPEC-13-shared-graphblas-build.md docs/specs/SPEC-14-lubm-rdfox-comparison.md \
  docs/specs/SPEC-17-metrics.md docs/specs/SPEC-19-streaming-runtime-pushdown.md \
  docs/specs/SPEC-28-named-graph-dataset-semantics.md
git commit -m "Point living specs' forward-looking sync checklists at Worklode"
```

---

### Task 6: Add the GitHub-issue-to-Worklode triage skill

**Files:**
- Create: `.claude/skills/triage-github-issue-for-worklode/SKILL.md`

**Context:** Worklode already has a real GitHub-issue-to-task pipeline: a
GitHub App webhook (or `lode inbox import` for backfill) drops issues into
the Worklode inbox as they're labeled; `lode inbox list` shows pending
(`new`) inbox items; `lode inbox promote --title ... --kind ... --priority
... [--body ... --applies-to ... --draft --parent ...]` turns one into a
real task; `lode inbox dismiss` closes one out with no task created; `lode
inbox link` attaches an inbox issue to a task that already exists (for
duplicates). Task kinds are `feature`, `bug`, `chore`, `design`, `review`,
`spike` (same enum as `lode task add --kind`). This skill is the judgment
layer on top of that pipeline: given one inbox item (a GitHub issue), decide
whether it's actionable, and if so, what kind, priority, and — for a bug —
whether it needs a `design` task first (unclear root cause / needs a design
decision) rather than going straight to a `bug` fix task.

- [ ] **Step 1: Read a sibling project skill for the house style**

Read `.claude/skills/nightly-benchmarks/SKILL.md` to match this repo's
existing skill format: YAML frontmatter (`name`, `description`) then a
single markdown body, no further front matter fields.

- [ ] **Step 2: Write the skill**

Create `.claude/skills/triage-github-issue-for-worklode/SKILL.md` with this
content:

````markdown
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

Each row is one untriaged GitHub issue. Read the issue itself (`gh issue
view <n>`) before deciding — the inbox list gives you the queue, not the
content.

## Decide: actionable, duplicate, or noise

For each item, first rule out the two non-task outcomes:

- **Duplicate of existing work** — an open Worklode task or another open
  issue already covers this. Attach it instead of creating a second task:
  ```bash
  lode inbox link <inbox-id> --task <existing-task-id>
  ```
- **Not actionable** — not a real bug (can't reproduce, works as intended,
  a support question, a duplicate that was already fixed), or too vague to
  act on without more information from the reporter. Dismiss it, and say
  why in the issue before dismissing (a dismissed inbox item creates no
  task, and the "why" only survives on the GitHub issue itself):
  ```bash
  gh issue comment <n> --body "<reason — ask a clarifying question, or explain why this isn't actionable>"
  lode inbox dismiss <inbox-id>
  ```

Everything else gets promoted into a task.

## Decide: what kind

Worklode's task kinds, and which fits a triaged issue:

| Kind | When an issue is this |
|---|---|
| `bug` | Reproducible wrong behavior with a known or discoverable cause. Goes straight to a fix. |
| `design` | The root cause is unclear, or fixing it correctly requires a decision this repo hasn't made yet (e.g. a genuine behavior/semantics question, not just "which function to edit"). File `design` first, not `bug` — the design task's output is the decision, and a follow-up `bug`/`feature` task (`child_of` the design task) does the actual fix once the decision is made. Don't skip this step for anything non-mechanical: a `bug` task with no clear fix just sits stuck in `in_progress`.
| `feature` | A new capability, not present today, requested by someone outside this loop. |
| `chore` | Maintenance with no user-visible behavior change (dependency bump, doc fix, dead-code removal, CI tweak). |
| `spike` | The issue asks a question this repo doesn't yet know the answer to (a feasibility check, a comparison, "can we even do X") rather than describing a fix or a feature. The spike's output is a finding, typically feeding a follow-up `design` or `feature` task. |
| `review` | The issue asks for a judgment call on something already written (a proposal, a design doc, a PR) rather than for new work. Rare from external GitHub issues; more common for internal-loop review requests. |

If unsure between `bug` and `design`, default to `design` — a fix task that
turns out to need a design decision costs more (an `in_progress` task
stalls, needs `task rework` or `task abandon` and a fresh `design` task) than
a design task that turns out to be quick (it just closes fast).

## Decide: priority and scope

Match this repo's existing priority taxonomy (from the retired `TASKS.md`,
still the working vocabulary): **critical** / **high** / **medium** / **low**.
Urgency, not effort — a five-minute fix to a crash is `critical`; a
multi-week feature nobody's blocked on is `low`. Check `docs/architecture.md`
for whether the affected subsystem is `implemented`, `planned`, or
`deferred` — an issue against `deferred` scope is almost never `critical`,
since Stage-2/3 work isn't expected yet.

If the issue clearly belongs to a bigger piece of work already tracked
(check `lode task list` and `docs/architecture.md`'s **Status** table),
promote it as a `child_of` that task rather than a bare top-level one:

```bash
lode inbox promote <inbox-id> \
  --kind bug --priority high \
  --title "<clear, specific title — not the issue's title verbatim if that's vague>" \
  --parent <existing-task-id>   # omit if this stands alone
```

Omit `--parent` for anything that doesn't clearly nest under existing work.
`--applies-to` links the task to the doc section it concerns (a SPEC or ADR)
when the issue is really about a documented design decision, not just code.

## After promoting

Tell the reporter what happened — comment on the GitHub issue with the new
task id and, for anything not `critical`/`high`, a rough sense of when it'll
be picked up (or that it's queued with no ETA):

```bash
gh issue comment <n> --body "Filed as a Worklode task (<task-id>, kind: <kind>, priority: <priority>). <one line on what happens next>."
```

Do not close the GitHub issue — Worklode's webhook closes it automatically
once the task's fix actually merges and lands on `main` (see the
`worklode` skill's webhook table). Closing it by hand here would desync the
two systems.
````

- [ ] **Step 3: Verify the file is well-formed**

Run: `head -5 .claude/skills/triage-github-issue-for-worklode/SKILL.md`
Expected: valid YAML frontmatter (`---` / `name:` / `description:` / `---`)
followed by the `# Triaging a GitHub issue for Worklode` heading.

- [ ] **Step 4: Commit**

```bash
git add .claude/skills/triage-github-issue-for-worklode/SKILL.md
git commit -m "Add triage-github-issue-for-worklode skill"
```

---

## Final check (run once, after all six tasks)

```bash
grep -rln "TASKS\.md" CLAUDE.md .claude/ docs/ | grep -v "^docs/plans/PLAN-" | \
  grep -vE "docs/specs/SPEC-(05|16|20|22)-"
```

Expected: no output. (Excludes are the deliberately-untouched historical
plan logs and the four "implemented"-status specs called out in Task 5.)
</content>
