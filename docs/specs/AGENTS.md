# Specs — agent instructions

These rules apply to every file in `docs/specs/` (except `README.md`, the index).

## Naming

1. Specs are named `SPEC-NN-<slug>.md` — two-digit `NN`, kebab-case slug.
   Take the next free number; never renumber an existing spec. This applies to
   *all* specs: standing subsystem contracts and narrower point/design specs
   alike (the frontmatter `scope:` line is what tells them apart, not the
   filename).

## Frontmatter

2. Every spec starts with YAML frontmatter carrying exactly these keys:

   ```yaml
   ---
   status: draft | approved | specified | implemented | roadmap | research-note
   date: YYYY-MM-DD        # the day the spec was written (not last edited)
   scope: "one line: what this spec covers"
   ---
   ```

   Keep `status:` current as the spec moves through its life — it should agree
   with the Status column in `README.md` and with `../architecture.md`.

## Housekeeping

- Add every new spec to the index table in `README.md` and to `../index.md` in
  the same commit.
- Implementation plans for a spec live in `../plans/` as
  `PLAN-NN-MM-<slug>.md`, where `NN` is this spec's number — see
  `../plans/AGENTS.md`.
- Subsystem contracts (SPEC-00..12) end with **Acceptance criteria** that gate
  the spec; the harness-first rule in the root `AGENTS.md` applies.
