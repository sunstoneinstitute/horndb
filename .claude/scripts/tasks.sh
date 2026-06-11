#!/usr/bin/env bash
#
# .claude/scripts/tasks.sh — lock-serialized (flock(2)) TASKS.md transitions
# for the /next-task workflow, with identifiable claims for orphan detection.
#
# Multiple /next-task agents share the *main* worktree's working tree and race
# on TASKS.md claim/complete lines. This script makes every TASKS.md mutation
# (and the matching `git add`/`commit`/`push origin main`) a single atomic,
# flock-guarded transaction so concurrent agents can never clobber each other or
# produce a half-written TASKS.md commit.
#
# It MUST be run from the MAIN worktree on branch `main`.
#
# Every claim records WHO / WHERE / WHEN as a structured tag on the index line:
#     — _wip: <session>@<host> · <branch> · <UTC-ISO-8601>_
# so an orphaned claim (dead session / crashed host) is identifiable and
# reapable — see the `claims` and `reap` subcommands.
#
# Subcommands
#   claim    --issue N --branch BR [--session S] [--title T] [--message MSG]
#       Flip the task referencing issue N from `[ ]` to `[v]` on BOTH its index
#       line and body heading, stamp the identity tag (above) on the index line,
#       commit, push, and print `claim_sha=<sha>`. Fails (exit 9) if the task is
#       not currently open — that failure IS the anti-collision check.
#       S defaults to ${CLAUDE_CODE_SESSION_ID:0:8} (or "unknown"); host is
#       `hostname`; the timestamp is UTC now.
#
#   complete --issue N [--title T] [--message MSG]
#       Flip `[v]` → `[x]` (claimed → done), strip the tag, commit, push.
#       Run only AFTER the PR has merged (closure stays merge-gated).
#
#   unclaim  --issue N [--title T] [--reason R] [--message MSG]
#       Flip `[v]` → `[ ]` (claimed → open), strip the tag, commit, push.
#       Use when abandoning a task, or to release an epic parent between
#       increments so the next increment is pickable.
#
#   claims   [--json]
#       List every active `[v]` claim with its issue, session, host, branch,
#       claim time, and age. Read-only (fetches + fast-forwards first so it
#       reflects origin/main). Drives orphan detection.
#
#   reap     --older-than DUR [--apply] [--message MSG]
#       Find claims older than DUR (e.g. 90m, 6h, 2d, or raw seconds). Without
#       --apply, just lists them (dry run). With --apply, releases them
#       (`[v]` → `[ ]`) in one locked commit — the orphan cleanup. A live agent
#       that is merely slow will re-detect its released claim is gone; size DUR
#       above your longest expected task to avoid reaping live work.
#
#   with-lock --message MSG -- CMD [ARGS...]
#       Escape hatch for free-form TASKS.md edits (epic breakdown notes, adding
#       or removing a task) that still need the lock. CMD runs INSIDE the lock
#       and must edit only TASKS.md; the script then commits + pushes it.
#
# Exit codes: 0 ok · 2 usage/guard · 3 dirty TASKS.md · 4 lock timeout ·
#             5 fast-forward failed · 9 task not in expected state.
#
# Env: TASKS_LOCK_TIMEOUT (seconds, default 180) · TASKS_REMOTE (default origin)
#      · TASKS_LOCK_TOOL (auto|flock|perl, default auto — auto prefers flock(1)
#        when installed and falls back to perl, so macOS needs no extra deps).
#
# TASKS.md carries no Rust, so these commits/pushes use `--no-verify` — the
# pre-commit (`cargo fmt --check`) and pre-push (workspace clippy + build) hooks
# are irrelevant here and would otherwise hold the lock for minutes. Code
# changes still go through the feature branch + PR + CI.

set -euo pipefail

SEP=" — _"          # tag separator on the line: space em-dash space underscore.
WIP="wip: "         # tag payload prefix, i.e. the line carries " — _wip: ...
                    # <session>@<host> · <branch> · <iso>_". This prefix never
                    # occurs elsewhere on a task line.

die() { echo "tasks.sh: $*" >&2; exit 2; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not in a git repository"

# --- Guard: must be the MAIN worktree, on branch main ---------------------
ABS_GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)"
COMMON_DIR="$(cd "$REPO_ROOT" && cd "$(git rev-parse --git-common-dir)" && pwd)"
[ "$ABS_GIT_DIR" = "$COMMON_DIR" ] || \
  die "must run from the MAIN worktree (this is a linked worktree: $ABS_GIT_DIR)"
BRANCH_NOW="$(git -C "$REPO_ROOT" branch --show-current)"
[ "$BRANCH_NOW" = "main" ] || die "main worktree must be on branch 'main' (currently '$BRANCH_NOW')"

TASKS="$REPO_ROOT/TASKS.md"
[ -f "$TASKS" ] || die "TASKS.md not found at $TASKS"
LOCKFILE="$COMMON_DIR/tasks.lock"
LOCK_TIMEOUT="${TASKS_LOCK_TIMEOUT:-180}"
REMOTE="${TASKS_REMOTE:-origin}"

# --- Parse subcommand + flags ---------------------------------------------
CMD="${1:-}"; shift || true
ISSUE=""; BRANCH=""; SESSION=""; TITLE=""; MESSAGE=""; REASON=""; TAG=""
OLDER_THAN=""; APPLY=0; JSON=0
declare -a RUNCMD=()
while [ $# -gt 0 ]; do
  case "$1" in
    --issue)      ISSUE="${2:?--issue needs a value}"; shift 2;;
    --branch)     BRANCH="${2:?--branch needs a value}"; shift 2;;
    --session)    SESSION="${2:?--session needs a value}"; shift 2;;
    --title)      TITLE="${2:?--title needs a value}"; shift 2;;
    --message)    MESSAGE="${2:?--message needs a value}"; shift 2;;
    --reason)     REASON="${2:?--reason needs a value}"; shift 2;;
    --tag)        TAG="${2:?--tag needs a value}"; shift 2;;
    --older-than) OLDER_THAN="${2:?--older-than needs a value}"; shift 2;;
    --apply)      APPLY=1; shift;;
    --json)       JSON=1; shift;;
    --)           shift; RUNCMD=("$@"); break;;
    *)            die "unknown flag '$1' (see header for usage)";;
  esac
done

[[ "$ISSUE" =~ ^[0-9]*$ ]] || die "--issue must be numeric (got '$ISSUE')"

# --- Duration "90m" / "6h" / "2d" / raw seconds -> seconds -----------------
to_seconds() {
  local d="$1"
  case "$d" in
    *d) echo $(( ${d%d} * 86400 ));;
    *h) echo $(( ${d%h} * 3600 ));;
    *m) echo $(( ${d%m} * 60 ));;
    *s) echo "${d%s}";;
    *)  [[ "$d" =~ ^[0-9]+$ ]] || die "bad --older-than '$d' (use 90m, 6h, 2d or seconds)"; echo "$d";;
  esac
}

# --- ISO-8601 UTC ("…Z") -> epoch seconds, GNU or BSD date ------------------
# Probed once: GNU date has -d; BSD/macOS date needs -j -u -f <fmt>. Prints 0
# when the timestamp is unparseable (legacy "?" tags), matching prior behavior.
if date -u -d "1970-01-01T00:00:00Z" +%s >/dev/null 2>&1; then
  iso_to_epoch() { date -u -d "$1" +%s 2>/dev/null || echo 0; }
else
  iso_to_epoch() { date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$1" +%s 2>/dev/null || echo 0; }
fi

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

# flip_checkbox ISSUE FROM TO STRIPTAG ADDTAG  -> writes $TASKS.tmp, exit 9 if
# no line in state FROM references /issues/ISSUE). ADDTAG (if non-empty) is the
# tag *payload* appended (wrapped as " — _<payload>_") to the FIRST matched
# (index) line only.
flip_checkbox() {
  local issue="$1" from="$2" to="$3" striptag="$4" addtag="$5"
  awk -v issue="$issue" -v from="$from" -v to="$to" \
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

case "$CMD" in
  claim)
    [ -n "$ISSUE" ]  || die "claim: --issue is required"
    [ -n "$BRANCH" ] || die "claim: --branch is required"
    require_tasks_clean
    ff_main
    sess="${SESSION:-${CLAUDE_CODE_SESSION_ID:0:8}}"; sess="${sess:-unknown}"
    host="$(hostname)"
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    tag="${TAG:-${WIP}${sess}@${host} · ${BRANCH} · ${ts}}"
    if ! flip_checkbox "$ISSUE" " " "v" "0" "$tag"; then
      rm -f "$TASKS.tmp"
      echo "tasks.sh: task #$ISSUE is not open ([ ]) on $REMOTE/main — already claimed/done, or no such task. Re-select." >&2
      exit 9
    fi
    mv "$TASKS.tmp" "$TASKS"
    commit_push "${MESSAGE:-chore(tasks): claim #$ISSUE${TITLE:+ ($TITLE)} [v] — $sess@$host}"
    echo "claim_sha=$(git -C "$REPO_ROOT" rev-parse HEAD)"
    ;;

  complete)
    [ -n "$ISSUE" ] || die "complete: --issue is required"
    require_tasks_clean
    ff_main
    if ! flip_checkbox "$ISSUE" "v" "x" "1" ""; then
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
    if ! flip_checkbox "$ISSUE" "v" " " "1" ""; then
      rm -f "$TASKS.tmp"
      echo "tasks.sh: task #$ISSUE is not claimed ([v]) on $REMOTE/main — nothing to unclaim." >&2
      exit 9
    fi
    mv "$TASKS.tmp" "$TASKS"
    commit_push "${MESSAGE:-chore(tasks): unclaim #$ISSUE${TITLE:+ ($TITLE)} [ ]${REASON:+ — $REASON}}"
    ;;

  claims)
    ff_main
    now="$(date -u +%s)"
    if [ "$JSON" = 1 ]; then
      first=1; printf '['
      while IFS=$'\t' read -r issue sess host branch iso; do
        epoch="$(iso_to_epoch "$iso")"
        age=$(( epoch > 0 ? now - epoch : -1 ))
        [ "$first" = 1 ] || printf ','; first=0
        printf '{"issue":%s,"session":"%s","host":"%s","branch":"%s","claimed":"%s","age_seconds":%s}' \
          "$issue" "$sess" "$host" "$branch" "$iso" "$age"
      done < <(parse_claims)
      printf ']\n'
    else
      printf '%-6s  %-12s  %-14s  %-34s  %-22s  %s\n' "ISSUE" "SESSION" "HOST" "BRANCH" "CLAIMED (UTC)" "AGE"
      any=0
      while IFS=$'\t' read -r issue sess host branch iso; do
        any=1
        epoch="$(iso_to_epoch "$iso")"
        if [ "$epoch" -gt 0 ]; then
          age=$(( now - epoch )); age_h="$(( age / 3600 ))h$(( (age % 3600) / 60 ))m"
        else age_h="?"; fi
        printf '%-6s  %-12s  %-14s  %-34s  %-22s  %s\n' "#$issue" "$sess" "$host" "$branch" "$iso" "$age_h"
      done < <(parse_claims)
      [ "$any" = 1 ] || echo "(no active claims)"
    fi
    ;;

  reap)
    [ -n "$OLDER_THAN" ] || die "reap: --older-than is required (e.g. 6h)"
    require_tasks_clean
    ff_main
    threshold="$(to_seconds "$OLDER_THAN")"
    now="$(date -u +%s)"
    declare -a STALE=()
    while IFS=$'\t' read -r issue sess host branch iso; do
      epoch="$(iso_to_epoch "$iso")"
      [ "$epoch" -gt 0 ] || continue
      age=$(( now - epoch ))
      if [ "$age" -ge "$threshold" ]; then
        STALE+=("$issue")
        printf 'stale: #%-5s %s@%s  branch=%s  claimed=%s  age=%sh\n' \
          "$issue" "$sess" "$host" "$branch" "$iso" "$(( age / 3600 ))"
      fi
    done < <(parse_claims)
    if [ "${#STALE[@]}" -eq 0 ]; then
      echo "reap: no claims older than $OLDER_THAN"
      exit 0
    fi
    if [ "$APPLY" != 1 ]; then
      echo "reap: ${#STALE[@]} stale claim(s) above (dry run — re-run with --apply to release)." >&2
      exit 0
    fi
    for issue in "${STALE[@]}"; do
      flip_checkbox "$issue" "v" " " "1" "" && mv "$TASKS.tmp" "$TASKS" || rm -f "$TASKS.tmp"
    done
    commit_push "${MESSAGE:-chore(tasks): reap ${#STALE[@]} stale claim(s) older than $OLDER_THAN [${STALE[*]}]}"
    echo "reap: released ${#STALE[@]} claim(s): ${STALE[*]}"
    ;;

  with-lock)
    [ -n "$MESSAGE" ] || die "with-lock: --message is required"
    [ "${#RUNCMD[@]}" -gt 0 ] || die "with-lock: a command must follow '--'"
    require_tasks_clean
    ff_main
    "${RUNCMD[@]}"
    others="$(git -C "$REPO_ROOT" status --porcelain | awk '{ p = substr($0, 4); if (p != "TASKS.md") print p }')"
    [ -z "$others" ] || die "with-lock: command touched files other than TASKS.md: $others"
    if git -C "$REPO_ROOT" diff --quiet -- TASKS.md && git -C "$REPO_ROOT" diff --cached --quiet -- TASKS.md; then
      die "with-lock: command made no change to TASKS.md"
    fi
    commit_push "$MESSAGE"
    ;;

  ""|-h|--help|help)
    sed -n '3,64p' "$0"
    [ "$CMD" = "" ] && exit 2 || exit 0
    ;;

  *)
    die "unknown subcommand '$CMD' (claim|complete|unclaim|claims|reap|with-lock|help)"
    ;;
esac
