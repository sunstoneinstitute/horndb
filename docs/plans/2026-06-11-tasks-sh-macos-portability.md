# tasks.sh macOS Portability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `.claude/scripts/tasks.sh` fully functional on macOS (issue #78): portable locking without requiring `flock(1)`, POSIX-awk claim parsing, and BSD-compatible claim-age computation — with a sandboxed regression test.

**Architecture:** Three surgical fixes inside the existing single-file script, plus a new self-contained test script that builds a throwaway git origin+clone sandbox and exercises the previously-silent failure paths (`claims`, `reap`, lock acquisition). No behavior change on Linux: `flock(1)` and GNU `date -d` remain the preferred paths when present.

**Tech Stack:** bash, POSIX awk, perl (ships on both Darwin and Linux; used only as the lock fallback), git.

---

## Background (read first)

`.claude/scripts/tasks.sh` serializes all TASKS.md transitions for the
multi-agent `/next-task` workflow. Three Linux-isms break it on macOS:

1. **`flock(1)` is required unconditionally** (line ~204). Stock Darwin has no
   `flock` binary — every subcommand dies with exit 4 after the lock timeout
   unless Homebrew's `flock` is installed.
2. **`parse_claims` uses gawk's 3-arg `match(s, re, arr)`** (lines ~181, ~193).
   BSD awk fails to *parse* the program, so `claims` prints an awk syntax error
   and "(no active claims)" even when claims exist, and `reap` can never find a
   stale claim. Silent orphan-detection failure.
3. **`date -d "$iso" +%s` is GNU-only** (three call sites: `claims` table,
   `claims --json`, `reap`). BSD `date` needs `-j -u -f <fmt>`.

The critical path (`claim`/`complete`/`unclaim`) only needs fix 1; fixes 2–3
restore `claims`/`reap`.

Key invariant to preserve: the lock must auto-release when the holding process
dies (crashed agent ⇒ no stale lock). `flock(2)` locks do this; a `mkdir`
spinlock does not. The perl fallback therefore flocks **the shell's inherited
FD 9** — when perl exits, the shell still holds the same open file
description, so the lock persists for the rest of the transaction and
evaporates if the shell dies.

---

### Task 1: Sandboxed regression test (red on macOS)

**Files:**
- Create: `.claude/scripts/test-tasks.sh` (mode 0755)

- [ ] **Step 1: Write the test script**

Create `.claude/scripts/test-tasks.sh` with exactly this content and `chmod +x` it:

```bash
#!/usr/bin/env bash
#
# .claude/scripts/test-tasks.sh — portability/regression tests for tasks.sh.
#
# Builds a throwaway sandbox (bare "origin" + clone acting as the main
# worktree) under mktemp and exercises the subcommands end-to-end, including
# the paths that silently broke on macOS (#78): `claims`/`reap` parsing
# (BSD awk), claim-age computation (BSD date), and lock acquisition without
# flock(1) (perl fallback). Never touches the real repository.
#
# Usage: .claude/scripts/test-tasks.sh   (exit 0 = all tests passed)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

PASS=0; FAIL=0
ok()   { PASS=$((PASS + 1)); echo "ok   - $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL - $1"; }

# --- Sandbox: bare origin + clone that satisfies tasks.sh's guards ----------
git init --quiet --bare "$SANDBOX/origin.git"
git clone --quiet "$SANDBOX/origin.git" "$SANDBOX/repo" 2>/dev/null
cd "$SANDBOX/repo"
git config user.email "test@example.invalid"
git config user.name "tasks.sh test"
git checkout --quiet -b main 2>/dev/null || git checkout --quiet main

# Fixture: one open task (#11) and one claim (#22) stamped 2026-01-01 (stale).
STALE_ISO="2026-01-01T00:00:00Z"
cat > TASKS.md <<EOF
# Tasks

## Index

- [ ] **LOW** · _Tooling_ — open task ([#11](https://github.com/example/repo/issues/11))
- [v] **LOW** · _Tooling_ — claimed task ([#22](https://github.com/example/repo/issues/22)) — _wip: cafe1234@testhost · task-22-branch · ${STALE_ISO}_

## Body

- [ ] **open task.** ([#11](https://github.com/example/repo/issues/11))

- [v] **claimed task.** ([#22](https://github.com/example/repo/issues/22))
EOF
mkdir -p .claude/scripts
cp "$SCRIPT_DIR/tasks.sh" .claude/scripts/tasks.sh
chmod +x .claude/scripts/tasks.sh
git add -A
git commit --quiet -m "init fixture"
git push --quiet -u origin main 2>/dev/null

T=.claude/scripts/tasks.sh

# --- 1. claims: parses the structured tag (BSD-awk regression, #78 item 2) --
out="$("$T" claims)"
echo "$out" | grep -q '#22'           && ok "claims lists issue #22"           || fail "claims lists issue #22"
echo "$out" | grep -q 'cafe1234'      && ok "claims parses session"            || fail "claims parses session"
echo "$out" | grep -q 'testhost'      && ok "claims parses host"               || fail "claims parses host"
echo "$out" | grep -q 'task-22-branch' && ok "claims parses branch"            || fail "claims parses branch"
echo "$out" | grep -q "$STALE_ISO"    && ok "claims parses claim timestamp"    || fail "claims parses claim timestamp"

# --- 2. claims: age is computed, not "?" (BSD-date regression, #78 item 3) --
age_field="$(echo "$out" | awk '/#22/ { print $NF }')"
[ "$age_field" != "?" ]               && ok "claims computes age (got $age_field)" || fail "claims computes age (got $age_field)"

json="$("$T" claims --json)"
age_s="$(echo "$json" | grep -o '"age_seconds":[0-9-]*' | head -1 | cut -d: -f2)"
[ -n "$age_s" ] && [ "$age_s" -gt 0 ] && ok "claims --json age_seconds > 0 (got ${age_s:-none})" \
                                      || fail "claims --json age_seconds > 0 (got ${age_s:-none})"

# --- 3. reap: detects + releases the stale claim ----------------------------
"$T" reap --older-than 12h | grep -q 'stale: #22' \
  && ok "reap dry-run finds stale #22" || fail "reap dry-run finds stale #22"
"$T" reap --older-than 12h --apply >/dev/null
grep -q '^- \[ \].*issues/22' TASKS.md \
  && ok "reap --apply releases #22"    || fail "reap --apply releases #22"

# --- 4. claim / double-claim / unclaim (critical path) -----------------------
out="$("$T" claim --issue 11 --branch task-11-test --session deadbeef --title "open task")"
echo "$out" | grep -q '^claim_sha='   && ok "claim prints claim_sha"           || fail "claim prints claim_sha"
grep -q '^- \[v\].*issues/11.*deadbeef' TASKS.md \
  && ok "claim stamps the index line"  || fail "claim stamps the index line"
rc=0; "$T" claim --issue 11 --branch other --session feedf00d >/dev/null 2>&1 || rc=$?
[ "$rc" = 9 ]                         && ok "double-claim exits 9"             || fail "double-claim exits 9 (got $rc)"
"$T" unclaim --issue 11 --title "open task" --reason test >/dev/null
grep -q '^- \[ \].*issues/11' TASKS.md \
  && ok "unclaim reopens the task"     || fail "unclaim reopens the task"

# --- 5. lock: perl fallback works; both tools mutually exclude (#78 item 1) --
TASKS_LOCK_TOOL=perl "$T" claims >/dev/null \
  && ok "perl lock fallback acquires"  || fail "perl lock fallback acquires"
perl -e '
  open(my $fh, ">>", ".git/tasks.lock") or die $!;
  use Fcntl qw(:flock); flock($fh, LOCK_EX) or die $!;
  sleep 4;' &
HOLDER=$!
sleep 1
rc=0; TASKS_LOCK_TOOL=perl TASKS_LOCK_TIMEOUT=1 "$T" claims >/dev/null 2>&1 || rc=$?
[ "$rc" = 4 ]                         && ok "perl lock times out when held"    || fail "perl lock times out when held (got $rc)"
if command -v flock >/dev/null 2>&1; then
  rc=0; TASKS_LOCK_TOOL=flock TASKS_LOCK_TIMEOUT=1 "$T" claims >/dev/null 2>&1 || rc=$?
  [ "$rc" = 4 ]                       && ok "flock excludes against perl holder" || fail "flock excludes against perl holder (got $rc)"
else
  echo "skip - flock(1) not installed; cross-tool exclusion not tested"
fi
wait "$HOLDER" 2>/dev/null || true

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" = 0 ]
```

- [ ] **Step 2: Run it and watch it fail on macOS (BSD awk/date paths)**

Run: `.claude/scripts/test-tasks.sh; echo "exit=$?"`

Expected on a stock-PATH macOS run (with Homebrew flock installed, as on this
machine): the `claims`-parsing tests FAIL (awk syntax error → "(no active
claims)"), the age tests FAIL, reap tests FAIL, and `TASKS_LOCK_TOOL=perl`
FAILS (env knob does not exist yet → unknown tool path doesn't exist; the
script ignores the var and uses flock — the test asserts via the knob, which
is added in Task 2). The claim/double-claim/unclaim tests should PASS
(critical path works with brew flock). Overall exit non-zero. **Red confirmed.**

- [ ] **Step 3: Commit the failing test**

```bash
git add .claude/scripts/test-tasks.sh
git commit -m "test: sandboxed regression tests for tasks.sh portability (#78)"
```

(Pre-commit hook only runs `cargo fmt --check`; shell files are unaffected.)

---

### Task 2: Portable lock acquisition (flock → perl fallback)

**Files:**
- Modify: `.claude/scripts/tasks.sh` (lock section, currently lines 202–207; header comment lines 54–62)

- [ ] **Step 1: Replace the lock acquisition block**

Replace:

```bash
# --- Acquire the lock for the whole transaction ---------------------------
exec 9>"$LOCKFILE"
if ! flock -w "$LOCK_TIMEOUT" 9; then
  echo "tasks.sh: could not acquire $LOCKFILE within ${LOCK_TIMEOUT}s" >&2
  exit 4
fi
```

with:

```bash
# --- Acquire the lock for the whole transaction ---------------------------
# flock(1) is not part of macOS. Prefer it when present, otherwise fall back
# to perl (ships on both Linux and Darwin): perl flocks the shell's inherited
# FD 9 and exits — the lock survives because this shell keeps the same open
# file description, and it still auto-releases if the shell dies. Both tools
# take the same flock(2) lock, so mixed fleets mutually exclude correctly.
# TASKS_LOCK_TOOL=auto|flock|perl overrides the probe (used by test-tasks.sh).
LOCK_TOOL="${TASKS_LOCK_TOOL:-auto}"
if [ "$LOCK_TOOL" = "auto" ]; then
  if command -v flock >/dev/null 2>&1; then LOCK_TOOL=flock; else LOCK_TOOL=perl; fi
fi
exec 9>"$LOCKFILE"
acquire_lock() {
  case "$LOCK_TOOL" in
    flock) flock -w "$LOCK_TIMEOUT" 9;;
    perl)
      perl -e '
        use Fcntl qw(:flock);
        open(my $fh, ">&=", 9) or die "tasks.sh: cannot adopt fd 9: $!\n";
        my $deadline = time() + $ARGV[0];
        until (flock($fh, LOCK_EX | LOCK_NB)) {
          exit 1 if time() >= $deadline;
          sleep 1;
        }' "$LOCK_TIMEOUT";;
    *) die "TASKS_LOCK_TOOL must be auto, flock or perl (got '$LOCK_TOOL')";;
  esac
}
if ! acquire_lock; then
  echo "tasks.sh: could not acquire $LOCKFILE within ${LOCK_TIMEOUT}s (lock tool: $LOCK_TOOL)" >&2
  exit 4
fi
```

- [ ] **Step 2: Update the header docs**

In the header comment, change the Env line (line ~57) to:

```
# Env: TASKS_LOCK_TIMEOUT (seconds, default 180) · TASKS_REMOTE (default origin)
#      · TASKS_LOCK_TOOL (auto|flock|perl, default auto — auto prefers flock(1)
#        when installed and falls back to perl, so macOS needs no extra deps).
```

- [ ] **Step 3: Run the lock tests**

Run: `.claude/scripts/test-tasks.sh 2>&1 | grep -E 'lock|flock'`
Expected: all three lock tests `ok` (perl fallback acquires; perl times out at
exit 4 when held; flock excludes against a perl holder).

- [ ] **Step 4: Commit**

```bash
git add .claude/scripts/tasks.sh
git commit -m "fix(tasks.sh): portable locking — fall back to perl flock when flock(1) is absent (#78)"
```

---

### Task 3: POSIX-awk `parse_claims` + portable ISO→epoch

**Files:**
- Modify: `.claude/scripts/tasks.sh` (`parse_claims`, lines ~177–200; new helper after `to_seconds`, lines ~113–123; three `date -d` call sites at lines ~261, ~273, ~291)

- [ ] **Step 1: Rewrite `parse_claims` in POSIX awk**

Replace the whole `parse_claims()` function with:

```bash
# Emit "issue<TAB>session<TAB>host<TAB>branch<TAB>iso" for each active claim.
# POSIX awk only (BSD awk has no 3-arg match()): capture via RSTART/RLENGTH,
# then split the tag payload on its " · " separators.
parse_claims() {
  awk -v sep="$SEP" -v wip="$WIP" '
    /^- \[v\]/ {
      if (!match($0, /\/issues\/[0-9]+\)/)) next
      issue = substr($0, RSTART + 8, RLENGTH - 9)   # strip "/issues/" and ")"
      # A task has two [v] lines (index + body heading); the index line comes
      # first and carries the tag. Emit one row per issue.
      if (issue in seen) next
      seen[issue] = 1
      sess = "?"; host = "?"; branch = "?"; iso = "?"
      marker = sep wip
      t = index($0, marker)
      if (t > 0) {
        payload = substr($0, t + length(marker))     # "<sess>@<host> · <branch> · <iso>_"
        sub(/_[ \t]*$/, "", payload)
        n = split(payload, parts, / · /)
        at = index(parts[1], "@")
        if (n == 3 && at > 0) {
          sess = substr(parts[1], 1, at - 1); host = substr(parts[1], at + 1)
          branch = parts[2]; iso = parts[3]
        } else { sess = payload }                    # legacy / free-form tag
      }
      print issue "\t" sess "\t" host "\t" branch "\t" iso
    }
  ' "$TASKS"
}
```

- [ ] **Step 2: Add the portable ISO→epoch helper**

Insert directly after the `to_seconds()` function:

```bash
# --- ISO-8601 UTC ("…Z") -> epoch seconds, GNU or BSD date ------------------
# Probed once: GNU date has -d; BSD/macOS date needs -j -u -f <fmt>. Prints 0
# when the timestamp is unparseable (legacy "?" tags), matching prior behavior.
if date -u -d "1970-01-01T00:00:00Z" +%s >/dev/null 2>&1; then
  iso_to_epoch() { date -u -d "$1" +%s 2>/dev/null || echo 0; }
else
  iso_to_epoch() { date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$1" +%s 2>/dev/null || echo 0; }
fi
```

- [ ] **Step 3: Replace the three GNU-only call sites**

In `claims` (both the `--json` branch and the table branch) and in `reap`,
replace each occurrence of:

```bash
epoch="$(date -d "$iso" +%s 2>/dev/null || echo 0)"
```

with:

```bash
epoch="$(iso_to_epoch "$iso")"
```

(Three occurrences total.)

- [ ] **Step 4: Update the header comment**

Header line 3 currently says "flock-serialized". Adjust the opening sentence
(lines 3–4) to not over-promise flock(1) specifically, e.g.:

```
# .claude/scripts/tasks.sh — lock-serialized (flock(2)) TASKS.md transitions
# for the /next-task workflow, with identifiable claims for orphan detection.
```

- [ ] **Step 5: Run the full test suite — all green**

Run: `.claude/scripts/test-tasks.sh; echo "exit=$?"`
Expected: every line `ok`, `failed: 0`, `exit=0`, on stock macOS awk/date.

- [ ] **Step 6: Cross-check with GNU tools if available (Linux parity)**

If `gawk` is installed locally, sanity-check the awk program parses under it:

```bash
gawk 'BEGIN { exit 0 }' >/dev/null 2>&1 && \
  PATH_PREFIX_TEST="$(mktemp -d)" && ln -s "$(command -v gawk)" "$PATH_PREFIX_TEST/awk" && \
  PATH="$PATH_PREFIX_TEST:$PATH" .claude/scripts/test-tasks.sh
```

Expected: still all green (POSIX constructs are valid gawk). If gawk is not
installed, note that and rely on POSIX conformance (2-arg match, RSTART,
RLENGTH, split with ERE are all POSIX awk).

- [ ] **Step 7: Commit**

```bash
git add .claude/scripts/tasks.sh
git commit -m "fix(tasks.sh): POSIX-awk claim parsing + BSD-compatible claim ages (#78)"
```

---

### Task 4: Real-repo smoke test + docs sync

**Files:**
- Modify: `docs/architecture.md` (only if a Status row covers #78 — check first; a LOW tooling task may have no row, in which case skip)

- [ ] **Step 1: Smoke-test against the real repository (read-only)**

From the **main worktree** (not this feature worktree — the script guards on
that), run the updated script read-only by pointing at the worktree copy:

```bash
cd "$(git rev-parse --git-common-dir)/.." 2>/dev/null || cd /Users/stig/git/sunstone/horndb
.worktrees/task-78-tasks-sh-macos-portability/.claude/scripts/tasks.sh claims
.worktrees/task-78-tasks-sh-macos-portability/.claude/scripts/tasks.sh reap --older-than 12h
```

Expected: no awk errors; the `claims` table shows the live #78 claim with a
real age; reap dry-run reports it (it is younger than 12h, so "no claims older
than 12h"). **Do not run any mutating subcommand from the worktree copy.**

- [ ] **Step 2: Check docs/architecture.md for a matching Status row**

Run: `grep -n -i "tasks.sh\|78" docs/architecture.md`
If a row tracks this task, flip its Status to **implemented** and commit with
message `docs(architecture): tasks.sh portability implemented (#78)`. If no
row exists (likely for a LOW tooling item), skip — TASKS.md bookkeeping
happens on `main` after the merge, per the /next-task workflow.

- [ ] **Step 3: Final verification**

```bash
.claude/scripts/test-tasks.sh
bash -n .claude/scripts/tasks.sh && bash -n .claude/scripts/test-tasks.sh
```

Expected: all tests green; both scripts parse clean.
