#!/usr/bin/env bash
#
# .claude/scripts/tasks.sh — flock-serialized TASKS.md transitions for the
# /next-task workflow.
#
# Multiple /next-task agents share the *main* worktree's working tree and race
# on TASKS.md claim/complete lines. This script makes every TASKS.md mutation
# (and the matching `git add`/`commit`/`push origin main`) a single atomic,
# flock-guarded transaction so concurrent agents can never clobber each other or
# produce a half-written TASKS.md commit.
#
# It MUST be run from the MAIN worktree on branch `main`: the transitions edit
# the main worktree's TASKS.md and commit/push to `main`. Running it from a
# linked worktree (or off `main`) is refused.
#
# Subcommands
#   claim    --issue N --tag "TEXT" [--title "T"] [--message "MSG"]
#       Flip the task referencing issue N from `[ ]` to `[v]` (open → claimed)
#       on BOTH its index line and body heading, append ` — _<TEXT>_` to the
#       index line, commit, push, and print `claim_sha=<sha>` on stdout. Fails
#       (exit 9) if the task is not currently open — that failure IS the
#       anti-collision check: the loser re-selects another task.
#
#   complete --issue N [--title "T"] [--message "MSG"]
#       Flip `[v]` → `[x]` (claimed → done), strip the wip tag, commit, push.
#       Run this only AFTER the PR has merged (closure stays merge-gated).
#
#   unclaim  --issue N [--title "T"] [--reason "R"] [--message "MSG"]
#       Flip `[v]` → `[ ]` (claimed → open), strip the wip tag, commit, push.
#       Use when abandoning a task, or to release an epic parent between
#       increments so the next increment is pickable.
#
#   with-lock --message "MSG" -- CMD [ARGS...]
#       Escape hatch for free-form TASKS.md edits (epic breakdown notes, adding
#       or removing a task) that still need the lock. Acquires the lock, fast-
#       forwards `main`, runs CMD (which must edit only TASKS.md), then commits
#       just TASKS.md with MSG and pushes. CMD runs INSIDE the lock, so the edit
#       is serialized with every other transition.
#
# Exit codes: 0 ok · 2 usage/guard · 3 dirty TASKS.md · 4 lock timeout ·
#             5 fast-forward failed · 9 task not in expected state.
#
# Env: TASKS_LOCK_TIMEOUT (seconds, default 180) · TASKS_REMOTE (default origin).
#
# Note: TASKS.md carries no Rust code, so these commits/pushes pass
# `--no-verify` — the pre-commit (`cargo fmt --check`) and pre-push (workspace
# `clippy` + `build`) hooks are irrelevant here and would otherwise hold the
# lock for minutes, serializing every other agent behind a build. Code changes
# still go through the feature branch + PR + CI.

set -euo pipefail

SEP=" — _"   # tag separator: space em-dash space underscore. The wrapped tag is
            # "<line> — _<text>_"; this prefix never occurs elsewhere on a task
            # line (titles use "— Title", categories use "· _Category_").

die() { echo "tasks.sh: $*" >&2; exit 2; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not in a git repository"

# --- Guard: must be the MAIN worktree, on branch main ---------------------
ABS_GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)"
COMMON_DIR="$(cd "$REPO_ROOT" && cd "$(git rev-parse --git-common-dir)" && pwd)"
[ "$ABS_GIT_DIR" = "$COMMON_DIR" ] || \
  die "must run from the MAIN worktree (this is a linked worktree: $ABS_GIT_DIR)"
BRANCH="$(git -C "$REPO_ROOT" branch --show-current)"
[ "$BRANCH" = "main" ] || die "main worktree must be on branch 'main' (currently '$BRANCH')"

TASKS="$REPO_ROOT/TASKS.md"
[ -f "$TASKS" ] || die "TASKS.md not found at $TASKS"
LOCKFILE="$COMMON_DIR/tasks.lock"
LOCK_TIMEOUT="${TASKS_LOCK_TIMEOUT:-180}"
REMOTE="${TASKS_REMOTE:-origin}"

# --- Parse subcommand + flags ---------------------------------------------
CMD="${1:-}"; shift || true
ISSUE=""; TAG=""; TITLE=""; MESSAGE=""; REASON=""
declare -a RUNCMD=()
while [ $# -gt 0 ]; do
  case "$1" in
    --issue)   ISSUE="${2:?--issue needs a value}"; shift 2;;
    --tag)     TAG="${2:?--tag needs a value}"; shift 2;;
    --title)   TITLE="${2:?--title needs a value}"; shift 2;;
    --message) MESSAGE="${2:?--message needs a value}"; shift 2;;
    --reason)  REASON="${2:?--reason needs a value}"; shift 2;;
    --)        shift; RUNCMD=("$@"); break;;
    *)         die "unknown flag '$1' (see header for usage)";;
  esac
done

[[ "$ISSUE" =~ ^[0-9]*$ ]] || die "--issue must be numeric (got '$ISSUE')"

# --- Helpers (run inside the lock) ----------------------------------------
require_tasks_clean() {
  if ! git -C "$REPO_ROOT" diff --quiet -- TASKS.md \
     || ! git -C "$REPO_ROOT" diff --cached --quiet -- TASKS.md; then
    echo "tasks.sh: TASKS.md has uncommitted changes — refusing (another op mid-flight?)." >&2
    exit 3
  fi
}

ff_main() {
  git -C "$REPO_ROOT" fetch --quiet "$REMOTE" main
  if ! git -C "$REPO_ROOT" merge --ff-only --quiet "$REMOTE/main"; then
    echo "tasks.sh: local 'main' is not a fast-forward of $REMOTE/main — reconcile first." >&2
    exit 5
  fi
}

commit_push() {  # $1 = commit message
  git -C "$REPO_ROOT" add -- TASKS.md
  git -C "$REPO_ROOT" commit --no-verify --quiet -m "$1" -- TASKS.md
  git -C "$REPO_ROOT" push --no-verify --quiet "$REMOTE" main
}

# flip_checkbox FROM TO STRIPTAG ADDTAG
#   FROM/TO  : single checkbox char (" ", v, x). Matches task lines whose
#              checkbox is FROM and that reference /issues/<ISSUE>).
#   STRIPTAG : "1" to remove a trailing " — _..._" tag from matched lines.
#   ADDTAG   : tag inner text to append (wrapped as " — _<text>_") to the FIRST
#              matched line (the index entry); "" to skip. Exits 9 if no line
#              in state FROM references issue ISSUE.
flip_checkbox() {
  local from="$1" to="$2" striptag="$3" addtag="$4"
  awk -v issue="$ISSUE" -v from="$from" -v to="$to" \
      -v striptag="$striptag" -v addtag="$addtag" -v sep="$SEP" '
    BEGIN { n = 0 }
    {
      line = $0
      prefix = "^- \\[" from "\\]"
      if ((line ~ prefix) && (index(line, "/issues/" issue ")") > 0)) {
        sub(prefix, "- [" to "]", line)
        if (striptag == "1") {
          p = index(line, sep)
          if (p > 0) line = substr(line, 1, p - 1)
        }
        if (addtag != "" && n == 0) {
          line = line sep addtag "_"
        }
        n++
      }
      print line
    }
    END { if (n == 0) exit 9 }
  ' "$TASKS" > "$TASKS.tmp"
}

# --- Acquire the lock for the whole transaction ---------------------------
exec 9>"$LOCKFILE"
if ! flock -w "$LOCK_TIMEOUT" 9; then
  echo "tasks.sh: could not acquire $LOCKFILE within ${LOCK_TIMEOUT}s" >&2
  exit 4
fi

case "$CMD" in
  claim)
    [ -n "$ISSUE" ] || die "claim: --issue is required"
    [ -n "$TAG" ]   || die "claim: --tag is required"
    require_tasks_clean
    ff_main
    if ! flip_checkbox " " "v" "0" "$TAG"; then
      rm -f "$TASKS.tmp"
      echo "tasks.sh: task #$ISSUE is not open ([ ]) on $REMOTE/main — already claimed/done, or no such task. Re-select." >&2
      exit 9
    fi
    mv "$TASKS.tmp" "$TASKS"
    commit_push "${MESSAGE:-chore(tasks): claim #$ISSUE${TITLE:+ ($TITLE)} [v]}"
    echo "claim_sha=$(git -C "$REPO_ROOT" rev-parse HEAD)"
    ;;

  complete)
    [ -n "$ISSUE" ] || die "complete: --issue is required"
    require_tasks_clean
    ff_main
    if ! flip_checkbox "v" "x" "1" ""; then
      rm -f "$TASKS.tmp"
      echo "tasks.sh: task #$ISSUE is not claimed ([v]) on $REMOTE/main — nothing to complete." >&2
      exit 9
    fi
    mv "$TASKS.tmp" "$TASKS"
    commit_push "${MESSAGE:-chore(tasks): complete #$ISSUE${TITLE:+ ($TITLE)} [x]}"
    ;;

  unclaim)
    [ -n "$ISSUE" ] || die "unclaim: --issue is required"
    require_tasks_clean
    ff_main
    if ! flip_checkbox "v" " " "1" ""; then
      rm -f "$TASKS.tmp"
      echo "tasks.sh: task #$ISSUE is not claimed ([v]) on $REMOTE/main — nothing to unclaim." >&2
      exit 9
    fi
    mv "$TASKS.tmp" "$TASKS"
    commit_push "${MESSAGE:-chore(tasks): unclaim #$ISSUE${TITLE:+ ($TITLE)} [ ]${REASON:+ — $REASON}}"
    ;;

  with-lock)
    [ -n "$MESSAGE" ] || die "with-lock: --message is required"
    [ "${#RUNCMD[@]}" -gt 0 ] || die "with-lock: a command must follow '--'"
    require_tasks_clean
    ff_main
    "${RUNCMD[@]}"
    # Only TASKS.md may have changed (porcelain path starts at column 4).
    others="$(git -C "$REPO_ROOT" status --porcelain | awk '{ p = substr($0, 4); if (p != "TASKS.md") print p }')"
    [ -z "$others" ] || die "with-lock: command touched files other than TASKS.md: $others"
    if git -C "$REPO_ROOT" diff --quiet -- TASKS.md && git -C "$REPO_ROOT" diff --cached --quiet -- TASKS.md; then
      die "with-lock: command made no change to TASKS.md"
    fi
    commit_push "$MESSAGE"
    ;;

  ""|-h|--help|help)
    sed -n '3,40p' "$0"
    [ "$CMD" = "" ] && exit 2 || exit 0
    ;;

  *)
    die "unknown subcommand '$CMD' (claim|complete|unclaim|with-lock|help)"
    ;;
esac
