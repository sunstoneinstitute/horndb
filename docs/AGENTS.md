# Docs agent instructions

These instructions apply to anyone editing files under `docs/`.

## Purpose

`docs/index.md` is both:

- a human index page for the docs directory, and
- a progressive-discovery map for coding agents.

Treat it as the front door to the docs tree.

## Rules

- Update `docs/index.md` in the same change whenever you add, remove, rename, or materially re-scope a docs file under `docs/`.
- Keep the index concise: one line per doc, with a short purpose statement and a clear next-read pointer when useful.
- Prefer shallow browsing over dumping everything into the index; deep detail belongs in the linked doc.
- If a doc grows into a distinct topic, split it into a new file and add the new file to the index.
- When a task touches query/update/reasoning behavior, make sure the index points the reader at the relevant spec or crate note before they start editing.

## architecture.md vs. architecture/

Keep these two separate — they answer different questions:

- `docs/architecture.md` is the single-page **status map**: one row per subsystem/feature with an implemented / specified / planned / deferred **Status**, kept in sync with `../TASKS.md`. It says *what exists today*, briefly.
- `docs/architecture/<subsystem>.md` holds per-subsystem **deep-dive guides** (e.g. `architecture/wcoj.md`): how a subsystem actually works, its invariants, and its gotchas. These say *how it works*, at length.

When you write a deep-dive, put it under `docs/architecture/`, link it from the index, and cross-link it from the relevant crate `AGENTS.md`/`INTEGRATION-NOTES.md`. Do not bloat the single-page map with deep-dive prose, and do not duplicate the status table inside a deep-dive.

## Good index shape

- Start here / orientation
- Docs in this directory
- Relevant specs and crate notes
- Where to go next for common tasks

## Writing style

- Use stable, descriptive titles.
- Put the one-sentence summary first.
- Avoid duplicating large chunks of content across multiple docs.
- If a doc is only for one subsystem, say so explicitly in the index.

## Progressive discovery reminder

The index should help both humans and agents answer:

- What is this doc for?
- When should I read it?
- What should I read next?

If the index cannot answer those quickly, it is too vague.
