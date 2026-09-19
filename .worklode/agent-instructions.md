<!-- worklode:begin — managed by `lode install`; edits inside are overwritten -->
## Worklode

This repository is tracked by Worklode (`lode`). Work is entered by
claiming a task:

- `lode work next [id]` claims the highest-ranked ready task, or the one
  named, and creates the worktree the work happens in.
- `lode task claim <id>` leases a task to a worktree that already
  exists — run it from inside that worktree, not the main checkout.
- `lode work resume <dir>` re-enters a worktree that already exists.
- `lode work status` reports the current worktree's task and lease.

The claimed task's brief carries the work itself; this block only says how to
reach it.
<!-- worklode:end -->
