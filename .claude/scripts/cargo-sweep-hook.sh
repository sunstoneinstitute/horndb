#!/usr/bin/env bash
# SessionStart hook: prune stale cargo build artifacts across horndb's
# worktrees. Runs a real (non-dry-run) sweep, keeping anything touched in
# the last 21 days -- safe for active work, since sccache (see AGENTS.md)
# makes re-fetching anything actually still needed cheap again.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$HOME/.cargo/env" 2>/dev/null
command -v cargo-sweep >/dev/null 2>&1 || exit 0

out=$(cargo sweep --recursive --hidden --time 21 "$REPO_ROOT" 2>&1)
cleaned=$(printf '%s\n' "$out" | grep -E "^\[INFO\] Cleaned [0-9]")

if [ -n "$cleaned" ]; then
  total=$(printf '%s\n' "$out" | grep "Total amount:" | tail -1 | sed 's/.*Total amount: //')
  if [ -z "$total" ]; then
    total=$(printf '%s\n' "$cleaned" | sed -E 's/^\[INFO\] Cleaned ([^ ]+ [^ ]+) from.*/\1/' | head -1)
  fi
  total_escaped=$(printf '%s' "$total" | sed 's/"/\\"/g')
  printf '{"systemMessage": "cargo-sweep freed %s of stale build artifacts across horndb worktrees"}' "$total_escaped"
fi
