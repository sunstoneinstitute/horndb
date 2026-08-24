---
description: Start a coding subagent that claims tasks via Worklode and executes them, in a loop
model: sonnet
argument-hint: "[task-id] [--project P] [--kind K] [--strict-focus]"
allowed-tools: Bash(lode *) Bash(git *)
---

Invocation arguments: $ARGUMENTS

Start a @lode-worker-agent with the instructions below:

Run `lode next --json`, adding only the invocation arguments that are among:

- `--project <key>`
- `--kind <kind>`
- `--strict-focus`

`lode next` takes at most one positional argument, so anything else the user
typed is context for the work, not command input — never pass it to the
command; pass it on to the subagent instead, and mention you did.

If `claimed` is false: tell the user nothing is ready and stop.
Otherwise a worktree was created and the lease is bound to it. cd into the
`worktree` path from the JSON, read the `brief`, and start the task. The brief
is the context contract — do NOT spelunk the repo to reconstruct context; if
the brief is insufficient, say so: the task likely needs decomposition.
Load the working-under-worklode skill before starting.
