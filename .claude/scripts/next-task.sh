#!/usr/bin/env bash
#
# .claude/scripts/next-task.sh — deterministic bootstrap for the /next-task
# workflow. Collapses Phases 0–4 (preflight → select → claim → worktree) into
# one scripted, non-AI step so an agent spends its first tool call getting a
# claimed task and a ready worktree, not shepherding a dozen shell commands.
#
# Selection is pure text processing over TASKS.md's `## Index` section:
# open `[ ]` items only, ordered priority-first (CRITICAL → HIGH → MEDIUM →
# LOW), then file order within a priority. Claiming goes through tasks.sh
# (flock-serialized, pushes to main); on a lost race (exit 9) the next
# candidate is tried automatically until one claim sticks or none are left.
#
# Subcommands
#   select   [--issue N]
#       Read-only dry run. Prints the ranked open candidates from the
#       working-tree TASKS.md (no fetch, no lock, runs anywhere) plus any
#       index lines that are open but unclaimable (no `/issues/N` link).
#
#   start    [--issue N] [--no-worktree]
#       The full bootstrap. Must run from the MAIN worktree on `main`:
#         1. preflight — clean tracked files (untracked only warns),
#            fetch + fast-forward main;
#         2. report active claims and a stale-claim dry run
#            (threshold $NEXT_TASK_REAP, default 12h — never auto-applies);
#         3. select the top open task (or --issue N), derive
#            branch `task-<N>-<slug>`, claim via tasks.sh (retry next
#            candidate on exit 9);
#         4. create `.worktrees/<branch>` forked from the claim commit, init
#            the GraphBLAS submodule, symlink .claude/CLAUDE.local.md if the
#            main worktree has one, rename the cmux tab (no-op outside cmux);
#         5. print a machine-readable context block (issue/priority/category/
#            title/branch/claim_sha/worktree/issue_url) followed by the
#            task's body section from TASKS.md.
#       With --no-worktree, stops after the claim (step 4 skipped).
#
#   abandon  --issue N --branch BR [--reason R]
#       The bail-out path: unclaim via tasks.sh, force-remove
#       `.worktrees/<BR>` and delete the local branch, rename the cmux tab.
#       Use only when the task did NOT reach a merged PR.
#
# Exit codes: 0 ok · 2 usage/guard · 7 no claimable open task ·
#             3/4/5 passed through from tasks.sh (dirty/lock/ff failure).
#
# Env: NEXT_TASK_REAP (stale-claim dry-run threshold, default 12h)
#      · CLAUDE_CODE_SESSION_ID (first 8 chars stamp the claim, via tasks.sh).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TASKS_SH="$SCRIPT_DIR/tasks.sh"

die() { echo "next-task.sh: $*" >&2; exit 2; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not in a git repository"
TASKS="$REPO_ROOT/TASKS.md"
[ -f "$TASKS" ] || die "TASKS.md not found at $TASKS"
[ -x "$TASKS_SH" ] || die "tasks.sh not found/executable at $TASKS_SH"

# --- Parse subcommand + flags ----------------------------------------------
CMD="${1:-}"; shift || true
ISSUE=""; BRANCH=""; REASON=""; NO_WORKTREE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --issue)       ISSUE="${2:?--issue needs a value}"; shift 2;;
    --branch)      BRANCH="${2:?--branch needs a value}"; shift 2;;
    --reason)      REASON="${2:?--reason needs a value}"; shift 2;;
    --no-worktree) NO_WORKTREE=1; shift;;
    *)             die "unknown flag '$1' (see header for usage)";;
  esac
done
[[ "$ISSUE" =~ ^[0-9]*$ ]] || die "--issue must be numeric (got '$ISSUE')"

# --- Candidate parsing (pure, over $TASKS) ---------------------------------
# Emits sorted "CAND<TAB>rank<TAB>lineno<TAB>issue<TAB>priority<TAB>category
# <TAB>title" rows for open `[ ]` index lines, and "SKIP<TAB>line" rows for
# open index lines with no `/issues/N` link (unclaimable via tasks.sh).
list_candidates() {
  awk '
    /^## Index/ { inx = 1; next }
    inx && /^## /  { inx = 0 }
    !inx { next }
    /^- \[ \] \*\*/ {
      line = $0
      # The tracking link is the LAST ([#N](…/issues/N)) on the line — index
      # lines may reference other issues inline before it.
      issue = ""; rest = line
      while (match(rest, /\/issues\/[0-9]+\)/)) {
        issue = substr(rest, RSTART + 8, RLENGTH - 9)
        rest = substr(rest, RSTART + RLENGTH)
      }
      if (issue == "") { printf "SKIP\t%s\n", line; next }
      if (!match(line, /\*\*(CRITICAL|HIGH|MEDIUM|LOW)\*\*/)) next
      pri = substr(line, RSTART + 2, RLENGTH - 4)
      rank = (pri == "CRITICAL" ? 0 : pri == "HIGH" ? 1 : pri == "MEDIUM" ? 2 : 3)
      cat = "?"
      if (match(line, /· _[A-Za-z]+_/)) {
        cat = substr(line, RSTART, RLENGTH)   # multibyte-safe: strip, dont offset
        gsub(/[^A-Za-z]/, "", cat)
      }
      t = line
      if (match(t, / — /)) t = substr(t, RSTART + RLENGTH)
      # Drop the trailing tracking link; keep any earlier inline links as text.
      p = index(t, " ([#" issue "]"); if (p > 0) t = substr(t, 1, p - 1)
      gsub(/\*\*/, "", t)
      # Cap runaway titles (some index lines carry paragraphs of status). Trim
      # at a space so a multibyte char is never split mid-sequence.
      if (length(t) > 160) {
        t = substr(t, 1, 157)
        if (match(t, / [^ ]*$/)) t = substr(t, 1, RSTART - 1)
        t = t "..."
      }
      printf "CAND\t%d\t%d\t%s\t%s\t%s\t%s\n", rank, NR, issue, pri, cat, t
    }
  ' "$TASKS" | sort -t "$(printf '\t')" -s -k1,1 -k2,2n -k3,3n
}

candidates_only() { list_candidates | grep '^CAND' || true; }
skipped_only()    { list_candidates | grep '^SKIP' || true; }

# task_body ISSUE — print the task's body section: the `- [` heading line that
# references /issues/ISSUE *after* the `## Index` section, plus its indented
# continuation lines, up to (exclusive) the next task bullet or `## ` heading.
task_body() {
  awk -v issue="$1" '
    /^## Index/ { inx = 1; next }
    inx { if (/^## /) inx = 0; else next }
    {
      if (printing) { if (/^- \[/ || /^## /) exit; print; next }
      if (/^- \[/ && index($0, "/issues/" issue ")") > 0) { printing = 1; print }
    }
  ' "$TASKS"
}

# issue_url ISSUE — the https://…/issues/N URL from the index line.
issue_url() {
  grep -o "https://[^)]*/issues/$1)" "$TASKS" | head -1 | sed 's/)$//' || true
}

# slug TITLE — kebab-case, ≤40 chars, no leading/trailing dash.
slug() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//' \
    | cut -c1-40 | sed -E 's/-+$//'
}

cmux_tab() {  # $1 = new tab title; silent no-op outside cmux
  if command -v cmux >/dev/null 2>&1 && [ -n "${CMUX_TAB_ID:-}" ]; then
    cmux rename-tab "$1" || true
  fi
}

print_candidate_table() {
  local any=0
  printf '%-6s  %-9s  %-16s  %s\n' "ISSUE" "PRIORITY" "CATEGORY" "TITLE"
  while IFS=$'\t' read -r _ _ _ issue pri cat title; do
    any=1
    printf '%-6s  %-9s  %-16s  %s\n' "#$issue" "$pri" "$cat" "$title"
  done < <(candidates_only)
  [ "$any" = 1 ] || echo "(no claimable open tasks)"
  local sk
  sk="$(skipped_only)"
  if [ -n "$sk" ]; then
    echo
    echo "open but unclaimable (no /issues/N link — file the issue first):"
    printf '%s\n' "$sk" | cut -f2- | sed 's/^/  /'
  fi
}

case "$CMD" in
  select)
    if [ -n "$ISSUE" ]; then
      row="$(candidates_only | awk -F '\t' -v i="$ISSUE" '$4 == i')"
      [ -n "$row" ] || { echo "next-task.sh: #$ISSUE is not an open claimable index task" >&2; exit 7; }
      printf '%s\n' "$row" | while IFS=$'\t' read -r _ _ _ issue pri cat title; do
        printf 'issue=%s\npriority=%s\ncategory=%s\ntitle=%s\n' "$issue" "$pri" "$cat" "$title"
      done
    else
      print_candidate_table
    fi
    ;;

  start)
    # --- Phase 0: preflight (main worktree, main branch, clean, up to date) --
    ABS_GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)"
    COMMON_DIR="$(cd "$REPO_ROOT" && cd "$(git rev-parse --git-common-dir)" && pwd)"
    [ "$ABS_GIT_DIR" = "$COMMON_DIR" ] || \
      die "start must run from the MAIN worktree (this is a linked worktree: $ABS_GIT_DIR)"
    BRANCH_NOW="$(git -C "$REPO_ROOT" branch --show-current)"
    [ "$BRANCH_NOW" = "main" ] || die "main worktree must be on branch 'main' (currently '$BRANCH_NOW')"
    if ! git -C "$REPO_ROOT" diff --quiet || ! git -C "$REPO_ROOT" diff --cached --quiet; then
      die "tracked files have uncommitted changes — commit/stash them first (not doing it for you)"
    fi
    untracked="$(git -C "$REPO_ROOT" status --porcelain | grep -c '^??' || true)"
    [ "$untracked" = 0 ] || echo "next-task.sh: note: $untracked untracked file(s) present (harmless, continuing)" >&2

    REMOTE="${TASKS_REMOTE:-origin}"
    git -C "$REPO_ROOT" fetch --quiet "$REMOTE" main
    git -C "$REPO_ROOT" merge --ff-only --quiet "$REMOTE/main" || {
      echo "next-task.sh: local 'main' is not a fast-forward of $REMOTE/main — reconcile first." >&2
      exit 5
    }

    # --- Orphan report (informational; never auto-applies a reap) ----------
    echo "=== active claims ==="
    "$TASKS_SH" claims
    echo
    echo "=== stale-claim dry run (>${NEXT_TASK_REAP:-12h}; release with: $TASKS_SH reap --older-than ${NEXT_TASK_REAP:-12h} --apply) ==="
    "$TASKS_SH" reap --older-than "${NEXT_TASK_REAP:-12h}"
    echo

    # --- Phases 1+3: select → claim, retrying next candidate on a lost race --
    TRIED=" "
    CLAIM_SHA=""; PICK_ISSUE=""; PICK_PRI=""; PICK_CAT=""; PICK_TITLE=""; PICK_BRANCH=""
    while :; do
      row=""
      while IFS=$'\t' read -r _ _ _ issue pri cat title; do
        case "$TRIED" in *" $issue "*) continue;; esac
        if [ -n "$ISSUE" ] && [ "$issue" != "$ISSUE" ]; then continue; fi
        row="$issue	$pri	$cat	$title"; break
      done < <(candidates_only)
      if [ -z "$row" ]; then
        if [ -n "$ISSUE" ]; then
          echo "next-task.sh: #$ISSUE is not an open claimable index task on $REMOTE/main" >&2
        else
          echo "next-task.sh: no claimable open [ ] tasks in TASKS.md — board is clear (or all open items lack issue links)." >&2
        fi
        exit 7
      fi
      IFS=$'\t' read -r issue pri cat title <<< "$row"
      br="task-$issue-$(slug "$title")"
      rc=0
      OUT="$("$TASKS_SH" claim --issue "$issue" --branch "$br" --title "$title")" || rc=$?
      if [ "$rc" = 0 ]; then
        CLAIM_SHA="${OUT#claim_sha=}"
        PICK_ISSUE="$issue"; PICK_PRI="$pri"; PICK_CAT="$cat"; PICK_TITLE="$title"; PICK_BRANCH="$br"
        break
      elif [ "$rc" = 9 ]; then
        echo "next-task.sh: lost the race for #$issue (claimed elsewhere) — trying the next candidate" >&2
        TRIED="$TRIED$issue "
        continue   # tasks.sh fast-forwarded main, so the re-parse sees fresh state
      else
        exit "$rc"   # dirty TASKS.md (3), lock timeout (4), ff failure (5), usage (2)
      fi
    done

    cmux_tab "[v] #$PICK_ISSUE"

    # --- Phase 4: worktree forked from the claim commit ---------------------
    WT_DIR=""
    if [ "$NO_WORKTREE" = 0 ]; then
      WT_DIR="$REPO_ROOT/.worktrees/$PICK_BRANCH"
      if [ -e "$WT_DIR" ] || git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$PICK_BRANCH"; then
        echo "next-task.sh: worktree/branch '$PICK_BRANCH' already exists — claim #$PICK_ISSUE HELD ($CLAIM_SHA)." >&2
        echo "next-task.sh: resolve manually, or release with: $0 abandon --issue $PICK_ISSUE --branch $PICK_BRANCH --reason 'bootstrap collision'" >&2
        exit 2
      fi
      if ! git -C "$REPO_ROOT" worktree add "$WT_DIR" -b "$PICK_BRANCH" "$CLAIM_SHA"; then
        echo "next-task.sh: worktree add FAILED — claim #$PICK_ISSUE is still held ($CLAIM_SHA)." >&2
        echo "next-task.sh: retry the worktree by hand, or release with: $0 abandon --issue $PICK_ISSUE --branch $PICK_BRANCH --reason 'worktree add failed'" >&2
        exit 2
      fi
      # GraphBLAS submodule is not checked out in a fresh worktree; closure's
      # build.rs panics without it.
      if git -C "$REPO_ROOT" config -f "$REPO_ROOT/.gitmodules" --get-regexp 'submodule\..*\.path' 2>/dev/null \
           | grep -q 'crates/closure/vendor/GraphBLAS'; then
        git -C "$WT_DIR" submodule update --init --recursive crates/closure/vendor/GraphBLAS
      fi
      # Share the one canonical gitignored local-instructions file, if present.
      if [ -f "$REPO_ROOT/.claude/CLAUDE.local.md" ]; then
        mkdir -p "$WT_DIR/.claude"
        ln -sfn ../../../.claude/CLAUDE.local.md "$WT_DIR/.claude/CLAUDE.local.md"
      fi
    fi

    # --- Context block: everything the agent needs to start Phase 5 ---------
    echo
    echo "=== next-task: claimed ==="
    echo "issue=$PICK_ISSUE"
    echo "priority=$PICK_PRI"
    echo "category=$PICK_CAT"
    echo "title=$PICK_TITLE"
    echo "issue_url=$(issue_url "$PICK_ISSUE")"
    echo "branch=$PICK_BRANCH"
    echo "claim_sha=$CLAIM_SHA"
    [ -n "$WT_DIR" ] && echo "worktree=$WT_DIR"
    echo "cargo_target_dir_hint=export CARGO_TARGET_DIR=\"$REPO_ROOT/../.horndb-shared-target\""
    case "$PICK_TITLE" in
      *EPIC*) echo "epic_hint=title contains EPIC — do the Phase-2 epic check (sub-issues, first increment) before implementing";;
    esac
    echo
    echo "=== task body (TASKS.md) ==="
    task_body "$PICK_ISSUE"
    ;;

  abandon)
    [ -n "$ISSUE" ]  || die "abandon: --issue is required"
    [ -n "$BRANCH" ] || die "abandon: --branch is required"
    "$TASKS_SH" unclaim --issue "$ISSUE" \
      ${REASON:+--reason "abandoned: $REASON"}
    git -C "$REPO_ROOT" worktree remove "$REPO_ROOT/.worktrees/$BRANCH" --force 2>/dev/null || true
    git -C "$REPO_ROOT" branch -D "$BRANCH" 2>/dev/null || true
    cmux_tab "#$ISSUE (released)"
    echo "abandoned: #$ISSUE released, worktree/branch '$BRANCH' removed"
    ;;

  ""|-h|--help|help)
    sed -n '3,46p' "$0"
    [ "$CMD" = "" ] && exit 2 || exit 0
    ;;

  *)
    die "unknown subcommand '$CMD' (select|start|abandon|help)"
    ;;
esac
