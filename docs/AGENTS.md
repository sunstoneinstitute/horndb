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

## Published docs (horndb.io)

`docs/ref/` and `docs/guides/` are the **only** directories published to
`horndb.io/docs/{reference,guides}/`. The authoritative include/exclude patterns
live in [`publish.toml`](publish.toml) — edit that to change the publish set, not
prose elsewhere. Everything not matched there (specs, plans, ADRs,
`architecture.md`, metrics, research) stays internal.

Prose and structure for those published pages follow their own house style, not
this file: see [`writing-style.md`](writing-style.md) — the four documentation
modes (Diátaxis), Oxford English spelling, code-sample rules, and reference-entry
templates. Read it before writing or editing anything under `docs/ref/` or
`docs/guides/`. This `AGENTS.md` governs the docs index and tree; `writing-style.md`
governs page content.

The site is a **Quarto** project: config in [`_quarto.yml`](_quarto.yml), the
Sunstone brand theme in `theme/` (light `horndb.scss` + dark `horndb-dark.scss`,
self-hosted fonts), tab icons in `head-extra.html` + `assets/`. It renders both
`.md` and `.qmd`; `.qmd` pages execute their code cells so example output is
real, not hand-typed. Build with `quarto render docs/`. The `_quarto.yml`
`render:` list must stay in step with `publish.toml`.
`.github/workflows/pages.yml` runs this render in CI on every push to `main`
that touches one of the docs paths it lists (`docs/ref/`, `docs/guides/`,
`docs/theme/`, `docs/assets/`, `docs/index.qmd`, `_quarto.yml`,
`head-extra.html`), and deploys the result under `horndb.io/docs/` alongside
the `site/` landing page in one GitHub Pages artifact — there is no separate
docs host. **Add any new top-level render input to that `paths:` list**, or
edits to it will silently never deploy.

`../site/` (the `horndb.io/` landing page) is a **second, separate Quarto
project**, not a subdirectory of this one — the two navbars differ too much to
share one config. What they do share is the compiled theme: `docs/theme/` is
canonical and `site/theme/` is a byte-identical duplicate (Quarto cannot follow
a symlink out of its own project directory). Change one, change both, or the
navbar/footer/toggle chrome drifts between `horndb.io/` and `horndb.io/docs/`.
The same duplication applies to the icon files in `assets/`. Beware the subtler
version of that drift: a page-level rule in `site/styles.css` (a `font-size` or
`line-height` on `body`) inherits into the shared chrome and shifts the navbar
by a pixel or two even though both compile from the same theme — set page text
metrics on `main`, not `body`.

A page opts into execution with `jupyter: python3` in its front matter (see
`guides/getting-started.qmd`). Rendering those pages needs a Python kernel:
create a venv once with `python3 -m venv docs/.venv && docs/.venv/bin/pip
install jupyter ipykernel` (self-gitignored, no project `.gitignore` entry
needed), then `source docs/.venv/bin/activate` before running `quarto render`.
Executable examples should drive the real product (the `serve` binary, the
Python binding) rather than print a hand-typed guess — see the "Keep the docs
honest" section of `writing-style.md`. If a cell launches a background
process (e.g. `serve`), it must terminate it reliably (`atexit` plus explicit
cleanup) — verify with `lsof`/`ps` after render that nothing leaks.

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
