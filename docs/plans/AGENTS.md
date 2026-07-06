# Plans — agent instructions

These rules apply to every file in `docs/plans/`.

## Naming

1. Plans are named `PLAN-NN-MM-<slug>.md`:
   - `NN` — two-digit number of the origin spec in `../specs/` (a spec
     typically produces more than one plan). Use `00` if the plan is not
     related to any spec.
   - `MM` — two-digit plan sequence number within that spec, in creation
     order. Take the next free `MM` for the spec; never renumber.
   - kebab-case slug.

## Frontmatter

2. Every plan starts with YAML frontmatter carrying exactly these keys:

   ```yaml
   ---
   status: draft | in-progress | executed | abandoned
   date: YYYY-MM-DD        # the day the plan was written
   scope: "one line: what this plan delivers"
   ---
   ```

   Flip `status:` to `executed` when the last task lands (same commit), or to
   `abandoned` with a one-line reason at the top of the body.

## Housekeeping

- Executed plans are historical implementation logs — commit-message-grade
  context, not a source of truth for current behaviour (the code and
  `../architecture.md` win).
- When a plan changes the outstanding work, update `TASKS.md` and
  `../architecture.md` in the same commit (sync rules in the root `AGENTS.md`).
