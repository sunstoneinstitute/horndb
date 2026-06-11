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
echo "$age_field" | grep -Eq '^[0-9]+h[0-9]+m$' \
  && ok "claims computes age (got $age_field)" || fail "claims computes age (got $age_field)"

json="$("$T" claims --json)"
age_s="$(echo "$json" | grep -o '"age_seconds":[0-9-]*' | head -1 | cut -d: -f2 || true)"
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
