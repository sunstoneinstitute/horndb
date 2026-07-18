#!/usr/bin/env bash
#
# .claude/scripts/test-next-task.sh — regression tests for next-task.sh.
#
# Builds a throwaway sandbox (bare "origin" + clone acting as the main
# worktree) under mktemp and exercises select / start / abandon end-to-end:
# priority-then-file-order selection, unclaimable-line reporting, claim +
# worktree bootstrap, forced --issue picks, body extraction, and the bail-out
# path. Never touches the real repository.
#
# Usage: .claude/scripts/test-next-task.sh   (exit 0 = all tests passed)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

PASS=0; FAIL=0
ok()   { PASS=$((PASS + 1)); echo "ok   - $1"; }
fail() { FAIL=$((FAIL + 1)); echo "FAIL - $1"; }

# --- Sandbox: bare origin + clone that satisfies the main-worktree guards ---
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
git init --quiet --bare "$SANDBOX/origin.git"
git clone --quiet "$SANDBOX/origin.git" "$SANDBOX/repo" 2>/dev/null
cd "$SANDBOX/repo"
git config user.email "test@example.invalid"
git config user.name "next-task.sh test"
git checkout --quiet -b main 2>/dev/null || git checkout --quiet main

# Fixture: file order deliberately disagrees with priority order (MEDIUM #33
# listed first), one claimed task (#22), one open line with no issue link.
cat > TASKS.md <<'EOF'
# Tasks

## Index

- [ ] **MEDIUM** · _Conformance_ — medium task listed first ([#33](https://github.com/example/repo/issues/33))
- [ ] **HIGH** · _Performance_ — high task: SPARQL speedup (12× gap, gated on [#99](https://github.com/example/repo/issues/99)) ([#11](https://github.com/example/repo/issues/11))
- [v] **CRITICAL** · _Completeness_ — **EPIC E1**: claimed epic ([#22](https://github.com/example/repo/issues/22)) — _wip: cafe1234@testhost · task-22-branch · 2026-01-01T00:00:00Z_
- [ ] **LOW** · _Maintainability_ — helper extraction with no issue yet (#TODO)
- [ ] **LOW** · _Tooling_ — low task ([#44](https://github.com/example/repo/issues/44))

## HIGH — Performance

- [ ] **high task: SPARQL speedup.** ([#11](https://github.com/example/repo/issues/11))
  Body line one for the high task.
  Body line two with the real scope.

- [ ] **medium task listed first.** ([#33](https://github.com/example/repo/issues/33))
  Medium body.

- [ ] **low task.** ([#44](https://github.com/example/repo/issues/44))
  Low body.
EOF
mkdir -p .claude/scripts
cp "$SCRIPT_DIR/tasks.sh" "$SCRIPT_DIR/next-task.sh" .claude/scripts/
chmod +x .claude/scripts/tasks.sh .claude/scripts/next-task.sh
git add -A
git commit --quiet -m "init fixture"
git push --quiet -u origin main 2>/dev/null

NT=.claude/scripts/next-task.sh

# --- 1. select: priority beats file order; claimed/linkless lines handled ---
out="$("$NT" select)"
first="$(echo "$out" | awk '/^#/ { print $1; exit }')"
[ "$first" = "#11" ]                    && ok "select ranks HIGH #11 first"      || fail "select ranks HIGH #11 first (got $first)"
order="$(echo "$out" | awk '/^#/ { printf "%s ", $1 }')"
[ "$order" = "#11 #33 #44 " ]           && ok "select order is #11 #33 #44"      || fail "select order is #11 #33 #44 (got '$order')"
echo "$out" | grep -q '#22'             && fail "select excludes claimed #22"    || ok "select excludes claimed #22"
echo "$out" | grep -q 'unclaimable'     && ok "select reports linkless line"     || fail "select reports linkless line"
echo "$out" | awk '$1 == "#99" { found = 1 } END { exit !found }' \
                                        && fail "inline issue links are not candidates" || ok "inline issue links are not candidates"
sel="$("$NT" select --issue 44)"
echo "$sel" | grep -q '^issue=44$'      && ok "select --issue 44 resolves"       || fail "select --issue 44 resolves"
echo "$sel" | grep -q '^category=Tooling$' \
                                        && ok "category has no markup"           || fail "category has no markup (got: $(echo "$sel" | grep ^category))"

# --- 2. start: claims the top task and bootstraps the worktree --------------
out="$("$NT" start)"
echo "$out" | grep -q '^issue=11$'      && ok "start picks #11"                  || fail "start picks #11"
echo "$out" | grep -q '^priority=HIGH$' && ok "start reports priority"           || fail "start reports priority"
branch="$(echo "$out" | sed -n 's/^branch=//p')"
echo "$branch" | grep -Eq '^task-11-high-task-sparql-speedup' \
                                        && ok "branch derived from slug ($branch)" || fail "branch derived from slug (got '$branch')"
sha="$(echo "$out" | sed -n 's/^claim_sha=//p')"
[ "$(git rev-parse HEAD)" = "$sha" ]    && ok "claim_sha is the claim commit"    || fail "claim_sha is the claim commit"
grep -q '^- \[v\].*issues/11' TASKS.md  && ok "start flips #11 to [v]"           || fail "start flips #11 to [v]"
wt="$(echo "$out" | sed -n 's/^worktree=//p')"
[ -d "$wt" ]                            && ok "worktree created"                 || fail "worktree created"
[ "$(git -C "$wt" branch --show-current)" = "$branch" ] \
                                        && ok "worktree is on the task branch"   || fail "worktree is on the task branch"
[ "$(git -C "$wt" rev-parse HEAD)" = "$sha" ] \
                                        && ok "worktree forked from claim_sha"   || fail "worktree forked from claim_sha"
echo "$out" | grep -q 'Body line two with the real scope' \
                                        && ok "start prints the task body"       || fail "start prints the task body"
echo "$out" | grep -q '^issue_url=https://github.com/example/repo/issues/11$' \
                                        && ok "start prints the issue url"       || fail "start prints the issue url"

# --- 3. start again: skips the claimed #11, takes #33 (no worktree) ---------
out="$("$NT" start --no-worktree)"
echo "$out" | grep -q '^issue=33$'      && ok "second start picks #33"           || fail "second start picks #33"
echo "$out" | grep -q '^worktree='      && fail "--no-worktree skips worktree"   || ok "--no-worktree skips worktree"

# --- 4. forced pick + epic hint ---------------------------------------------
"$NT" abandon --issue 33 --branch task-33-medium-task-listed-first --reason test >/dev/null
out="$("$NT" start --issue 44 --no-worktree)"
echo "$out" | grep -q '^issue=44$'      && ok "start --issue 44 forces the pick" || fail "start --issue 44 forces the pick"
rc=0; "$NT" start --issue 22 --no-worktree >/dev/null 2>&1 || rc=$?
[ "$rc" = 7 ]                           && ok "start --issue on claimed task exits 7" || fail "start --issue on claimed task exits 7 (got $rc)"

# --- 5. abandon: releases the claim and removes the worktree ----------------
"$NT" abandon --issue 11 --branch "$branch" --reason "test bail-out" >/dev/null
grep -q '^- \[ \].*issues/11' TASKS.md  && ok "abandon reopens #11"              || fail "abandon reopens #11"
[ ! -d ".worktrees/$branch" ]           && ok "abandon removes the worktree"     || fail "abandon removes the worktree"
git show-ref --verify --quiet "refs/heads/$branch" \
                                        && fail "abandon deletes the branch"     || ok "abandon deletes the branch"

# --- 6. board drained: everything claimed/absent -> exit 7 ------------------
"$NT" start --no-worktree >/dev/null    # takes #11 again
"$NT" start --no-worktree >/dev/null    # takes #44
rc=0; "$NT" start --no-worktree >/dev/null 2>&1 || rc=$?
[ "$rc" = 7 ]                           && ok "drained board exits 7"            || fail "drained board exits 7 (got $rc)"

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" = 0 ]
