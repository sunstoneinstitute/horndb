#!/usr/bin/env bash
#
# .claude/scripts/tasks-github.sh — Option B PROTOTYPE: GitHub issues as the
# source of truth for the task list, with TASKS.md as a generated read-only view.
#
# Why this exists: TASKS.md-in-git makes volatile claim state merge-hostile, so
# the file-based flow (.claude/scripts/tasks.sh) needs an flock + careful merge
# handling. Option B moves task *state* onto the GitHub issue itself:
#
#   * priority / category      -> existing `priority: …` / `category: …` labels
#   * open / claimed / review  -> `status:in-progress` / `status:in-review` labels
#   * done                     -> issue closed (via the PR's `Closes #N`)
#   * who/where/when claimed    -> a structured claim *comment* (GitHub stamps
#                                 the time; we add session+host+branch)
#
# Because every agent authenticates as the SAME GitHub user (`stigsb`), assignee
# can't arbitrate a race. Instead claiming uses a **comment-id election**: the
# claimant posts a marker comment, then the lowest GitHub comment id among the
# active marker comments wins (comment ids are globally monotonic, so this is a
# total order with no ties — correct across hosts, no local lock required).
#
# Subcommands (mirror tasks.sh so /next-task barely changes):
#   claim    --issue N --branch BR [--session S] [--repo R]
#       Claim issue N: post the marker comment, add `status:in-progress`, run the
#       election. Wins -> prints `base_sha=<origin/main>` (the worktree forks
#       from there, since there is no claim commit). Loses or already claimed
#       -> exit 9 (re-select).
#   complete --issue N [--repo R]
#       Drop `status:in-progress` (the issue itself closes via the merged PR's
#       `Closes #N`). Run after merge.
#   unclaim  --issue N [--reason R] [--repo R]
#       Release: drop `status:in-progress` and delete this claim's marker comment.
#   claims   [--json] [--repo R]
#       List active claims (issues with `status:in-progress`) with
#       issue/session/host/branch/claimed-at/age — orphan detection.
#   reap     --older-than DUR [--apply] [--repo R]
#       Find claims older than DUR (90m/6h/2d/seconds); --apply releases them.
#   render   [--out FILE] [--repo R]
#       Regenerate a read-only TASKS.md from `gh issue list`, grouped by
#       priority, marking claimed / in-review. Default --out is TASKS.md.
#
# Status: PROTOTYPE for evaluating Option B alongside the file-based tasks.sh.
# Requires `gh` authenticated with repo write scope. Exit codes mirror tasks.sh
# (0 ok · 2 usage · 9 not-claimable).

set -euo pipefail

CLAIM_LABEL="status:in-progress"
REVIEW_LABEL="status:in-review"
MARKER="<!-- tasks-claim v1 -->"
PRIO_ORDER=("priority: critical" "priority: high" "priority: medium" "priority: low")

die() { echo "tasks-github.sh: $*" >&2; exit 2; }
need() { command -v "$1" >/dev/null || die "missing dependency: $1"; }
need gh

CMD="${1:-}"; shift || true
ISSUE=""; BRANCH=""; SESSION=""; REASON=""; REPO=""; OLDER_THAN=""; OUT=""
APPLY=0; JSON=0
while [ $# -gt 0 ]; do
  case "$1" in
    --issue)      ISSUE="${2:?}"; shift 2;;
    --branch)     BRANCH="${2:?}"; shift 2;;
    --session)    SESSION="${2:?}"; shift 2;;
    --reason)     REASON="${2:?}"; shift 2;;
    --repo)       REPO="${2:?}"; shift 2;;
    --older-than) OLDER_THAN="${2:?}"; shift 2;;
    --out)        OUT="${2:?}"; shift 2;;
    --apply)      APPLY=1; shift;;
    --json)       JSON=1; shift;;
    *)            die "unknown flag '$1'";;
  esac
done

REPO="${REPO:-${TASKS_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)}}"
[ -n "$REPO" ] || die "could not determine repo (pass --repo owner/name)"

to_seconds() {
  local d="$1"
  case "$d" in
    *d) echo $(( ${d%d} * 86400 ));; *h) echo $(( ${d%h} * 3600 ));;
    *m) echo $(( ${d%m} * 60 ));;    *s) echo "${d%s}";;
    *)  [[ "$d" =~ ^[0-9]+$ ]] || die "bad --older-than '$d'"; echo "$d";;
  esac
}

# Numeric comment id from a comment URL (…#issuecomment-<id>).
comment_id_from_url() { sed -n 's/.*#issuecomment-\([0-9]\+\).*/\1/p' <<<"$1"; }

# Print "id<TAB>body" for each active marker comment on $1 (issue number).
marker_comments() {
  gh issue view "$1" --repo "$REPO" --json comments \
    --jq ".comments[] | select(.body | contains(\"$MARKER\")) | \"\(.url)\t\(.body)\"" \
  | while IFS=$'\t' read -r url body; do
      printf '%s\t%s\n' "$(comment_id_from_url "$url")" "$body"
    done
}

issue_state() { gh issue view "$1" --repo "$REPO" --json state -q .state; }
has_label()   { gh issue view "$1" --repo "$REPO" --json labels -q '.labels[].name' | grep -qxF "$2"; }

# Parse "session=… host=… branch=… at=…" out of a marker comment body.
field() { sed -n "s/.*$1=\\([^ ]*\\).*/\\1/p" <<<"$2"; }

case "$CMD" in
  claim)
    [ -n "$ISSUE" ] && [ -n "$BRANCH" ] || die "claim: --issue and --branch required"
    [ "$(issue_state "$ISSUE")" = "OPEN" ] || die "claim: #$ISSUE is not OPEN"
    if has_label "$ISSUE" "$CLAIM_LABEL"; then
      echo "tasks-github.sh: #$ISSUE already claimed (status:in-progress) — see 'claims'/'reap'. Re-select." >&2
      exit 9
    fi
    sess="${SESSION:-${CLAUDE_CODE_SESSION_ID:0:8}}"; sess="${sess:-unknown}"
    host="$(hostname)"; ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    # No backticks/markup around the fields: keep them bare so `field()` parses
    # each as a non-space token (the trailing `at=` must reach end-of-line clean).
    body="$MARKER 🔒 claimed session=$sess host=$host branch=$BRANCH at=$ts"
    url="$(gh issue comment "$ISSUE" --repo "$REPO" --body "$body")"
    myid="$(comment_id_from_url "$url")"
    gh issue edit "$ISSUE" --repo "$REPO" --add-label "$CLAIM_LABEL" >/dev/null
    # Election: lowest active marker-comment id wins.
    winner="$(marker_comments "$ISSUE" | cut -f1 | sort -n | head -1)"
    if [ "$winner" = "$myid" ]; then
      base="$(git rev-parse origin/main 2>/dev/null || true)"
      echo "claimed #$ISSUE as $sess@$host (branch $BRANCH)"
      [ -n "$base" ] && echo "base_sha=$base"
    else
      gh api -X DELETE "/repos/$REPO/issues/comments/$myid" >/dev/null   # retract; winner keeps the label
      echo "tasks-github.sh: lost claim race for #$ISSUE to an earlier claimant. Re-select." >&2
      exit 9
    fi
    ;;

  complete)
    [ -n "$ISSUE" ] || die "complete: --issue required"
    gh issue edit "$ISSUE" --repo "$REPO" --remove-label "$CLAIM_LABEL" --remove-label "$REVIEW_LABEL" >/dev/null 2>&1 || true
    echo "completed #$ISSUE (status labels cleared; issue closes via the merged PR's Closes #$ISSUE)"
    ;;

  unclaim)
    [ -n "$ISSUE" ] || die "unclaim: --issue required"
    gh issue edit "$ISSUE" --repo "$REPO" --remove-label "$CLAIM_LABEL" >/dev/null 2>&1 || true
    marker_comments "$ISSUE" | cut -f1 | while read -r id; do
      [ -n "$id" ] && gh api -X DELETE "/repos/$REPO/issues/comments/$id" >/dev/null 2>&1 || true
    done
    [ -n "$REASON" ] && gh issue comment "$ISSUE" --repo "$REPO" --body "released claim: $REASON" >/dev/null || true
    echo "unclaimed #$ISSUE"
    ;;

  claims|reap)
    now="$(date -u +%s)"
    [ "$CMD" = reap ] && { [ -n "$OLDER_THAN" ] || die "reap: --older-than required"; threshold="$(to_seconds "$OLDER_THAN")"; }
    # List claimed issues via REST (the `-f` form lets gh url-encode the label).
    # The claim *election* never depends on this list — it reads the issue's own
    # comments, which are immediately consistent. This list only feeds claims/
    # reap, where a few seconds of label-index lag on a *just*-claimed issue is
    # harmless (orphans are old claims; the index is long settled). PRs filtered.
    mapfile -t CLAIMED < <(gh api --paginate -X GET "/repos/$REPO/issues" \
        -f state=open -f labels="$CLAIM_LABEL" -f per_page=100 \
        --jq '.[] | select(has("pull_request") | not) | .number')
    [ "$CMD" = claims ] && [ "$JSON" != 1 ] && \
      printf '%-6s  %-12s  %-14s  %-30s  %-22s  %s\n' ISSUE SESSION HOST BRANCH "CLAIMED (UTC)" AGE
    [ "$JSON" = 1 ] && { printf '['; first=1; }
    declare -a STALE=()
    for n in "${CLAIMED[@]:-}"; do
      [ -n "$n" ] || continue
      body="$(marker_comments "$n" | tail -1 | cut -f2)"
      sess="$(field session "$body")"; host="$(field host "$body")"
      br="$(field branch "$body")";   at="$(field at "$body")"
      [ -n "$sess" ] || sess="?"; [ -n "$host" ] || host="?"; [ -n "$br" ] || br="?"; [ -n "$at" ] || at="?"
      epoch="$(date -d "$at" +%s 2>/dev/null || echo 0)"
      age=$(( epoch > 0 ? now - epoch : -1 ))
      if [ "$CMD" = claims ]; then
        if [ "$JSON" = 1 ]; then
          [ "$first" = 1 ] || printf ','; first=0
          printf '{"issue":%s,"session":"%s","host":"%s","branch":"%s","claimed":"%s","age_seconds":%s}' \
            "$n" "$sess" "$host" "$br" "$at" "$age"
        else
          ageh=$([ "$age" -ge 0 ] && echo "$(( age/3600 ))h$(( (age%3600)/60 ))m" || echo "?")
          printf '%-6s  %-12s  %-14s  %-30s  %-22s  %s\n' "#$n" "$sess" "$host" "$br" "$at" "$ageh"
        fi
      else  # reap
        if [ "$epoch" -gt 0 ] && [ "$age" -ge "$threshold" ]; then
          STALE+=("$n")
          printf 'stale: #%-5s %s@%s branch=%s claimed=%s age=%sh\n' "$n" "$sess" "$host" "$br" "$at" "$(( age/3600 ))"
        fi
      fi
    done
    if [ "$CMD" = claims ]; then
      [ "$JSON" = 1 ] && printf ']\n'
      [ "$JSON" != 1 ] && [ "${#CLAIMED[@]}" -eq 0 ] && echo "(no active claims)"
    else
      if [ "${#STALE[@]}" -eq 0 ]; then echo "reap: no claims older than $OLDER_THAN"; exit 0; fi
      if [ "$APPLY" != 1 ]; then echo "reap: ${#STALE[@]} stale (dry run — re-run with --apply)."; exit 0; fi
      for n in "${STALE[@]}"; do
        gh issue edit "$n" --repo "$REPO" --remove-label "$CLAIM_LABEL" >/dev/null 2>&1 || true
        marker_comments "$n" | cut -f1 | while read -r id; do
          [ -n "$id" ] && gh api -X DELETE "/repos/$REPO/issues/comments/$id" >/dev/null 2>&1 || true
        done
      done
      echo "reap: released ${#STALE[@]} claim(s): ${STALE[*]}"
    fi
    ;;

  render)
    OUT="${OUT:-TASKS.md}"
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    tmp="$(mktemp)"
    {
      echo "# Tasks — generated from GitHub issues (DO NOT EDIT)"
      echo
      echo "> Generated by \`.claude/scripts/tasks-github.sh render\` at $ts."
      echo "> **Source of truth: GitHub issues** ($REPO). Claim/complete via the script;"
      echo "> edit task detail on the issue, not here. \`🔒\` = claimed, \`👀\` = in review."
      echo
      issues_json="$(gh issue list --repo "$REPO" --state open --limit 300 \
        --json number,title,labels)"
      for prio in "${PRIO_ORDER[@]}"; do
        rows="$(jq -r --arg p "$prio" '
          [ .[] | select(any(.labels[].name; . == $p)) ] | sort_by(.number) | .[] |
          { n:.number, t:.title,
            cat:( [ .labels[].name | select(startswith("category: ")) ] | (.[0] // "") | sub("category: ";"") ),
            claimed:( any(.labels[].name; . == "status:in-progress") ),
            review:( any(.labels[].name; . == "status:in-review") ) } |
          "- [ ] #\(.n) \(.t)\( if .cat != "" then " — _\(.cat)_" else "" end )\( if .claimed then " · 🔒" else "" end )\( if .review then " · 👀" else "" end )"
        ' <<<"$issues_json")"
        [ -z "$rows" ] && continue
        label="${prio#priority: }"
        echo "## ${label^^}"
        echo
        echo "$rows"
        echo
      done
      # Issues with no priority label.
      none="$(jq -r '
        [ .[] | select(any(.labels[].name; startswith("priority: ")) | not) ] | sort_by(.number) | .[] |
        "- [ ] #\(.number) \(.title)" ' <<<"$issues_json")"
      if [ -n "$none" ]; then echo "## UNPRIORITIZED"; echo; echo "$none"; echo; fi
      echo "_Done = closed on GitHub; not shown. Run \`gh issue list --state closed\` for history._"
    } > "$tmp"
    if [ "$OUT" = "-" ]; then cat "$tmp"; rm -f "$tmp"
    else mv "$tmp" "$OUT"; echo "rendered $REPO open issues -> $OUT"; fi
    ;;

  ""|-h|--help|help)
    sed -n '3,55p' "$0"
    [ "$CMD" = "" ] && exit 2 || exit 0;;
  *)
    die "unknown subcommand '$CMD' (claim|complete|unclaim|claims|reap|render|help)";;
esac
