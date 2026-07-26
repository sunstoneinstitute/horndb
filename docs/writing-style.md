# HornDB documentation writing style

House style for the **published, user-facing** HornDB docs: what goes on
`horndb.io/docs/`. It sets the structure (how pages are organized) and the prose
style (voice, spelling, code samples, links) so every page reads as one product.

Read this before writing or editing anything under `docs/ref/` or
`docs/guides/`. If you are writing a spec, a plan, or the docs index instead, this
file does **not** govern that — see [Scope](#scope).

## Scope

[`docs/publish.toml`](publish.toml) is the source of truth for what reaches
`horndb.io/docs/`. Today it publishes two source directories and nothing else:

| Source | Published at | Holds |
|---|---|---|
| `docs/ref/` | `horndb.io/docs/reference/` | Reference pages — facts you consult while working. |
| `docs/guides/` | `horndb.io/docs/guides/` | Tutorials, how-to guides, and concept/explanation pages. |

The include/exclude globs live in `docs/publish.toml`, not in this prose, so the
publish set can change without editing the style guide. Anything not matched there
— specs, plans, ADRs, `architecture.md`, `metrics.md`, research notes — stays
internal. Do not link a published page to an internal one; a reader on `horndb.io`
cannot follow it. Pull any fact you need from an internal doc into the published
page itself.

This guide covers the published docs only. Two neighbouring style rules exist and
are **not** superseded here:

- **Specs, plans, and internal docs** — the "Writing Style: Plain Language,
  Precise Meaning" rules in the root `CLAUDE.md`. The published docs inherit that
  plain-language discipline; this file adds the extra rules a user-facing manual
  needs (the four modes, Oxford spelling, reference templates).
- **The docs index and tree** — `docs/AGENTS.md`. That governs `docs/index.md`,
  not page prose.

## The four modes — organize by what the reader needs

Every published page serves exactly **one** of four reader needs. This is the
[Diátaxis](https://diataxis.fr/) framework. The one rule that matters most:
**never mix two modes in one page.** A tutorial that stops to explain internals,
or a reference entry that starts teaching, fails at both jobs. When a page feels
wrong, it is usually two modes bleeding together — split it.

| Mode | Reader is… | The page's one job | Lives in |
|---|---|---|---|
| **Tutorial** | learning by doing | Take a beginner along a guided path to a first success. | `docs/guides/` |
| **How-to guide** | working towards a goal | Give a competent user the steps to a real-world result. | `docs/guides/` |
| **Reference** | looking a fact up | State how the machinery behaves. Describe, do not teach. | `docs/ref/` |
| **Explanation** | trying to understand | Discuss *why* — design, trade-offs, background. | `docs/guides/` |

To classify a page, ask two questions: does it serve **action** (doing) or
**cognition** (thinking), and does it serve **study** (learning) or **work**
(getting a task done)? Tutorial = action + study. How-to = action + work.
Reference = cognition + work. Explanation = cognition + study.

The split maps onto the two published directories like this:

- `docs/ref/` holds **only** reference pages.
- `docs/guides/` holds the three narrative modes — tutorials, how-to guides, and
  explanation. Even here, one page stays in one mode: a how-to that grows a "why"
  section moves that section to a separate explanation page and links to it.

### Tutorial — the strictest mode

A tutorial must **work for every reader, every time**. A step that fails makes the
learner lose faith in the tutorial and in themselves. So:

- **One path, no choices.** Give a single concrete route to the finish. No
  alternative flags, no "you could also…". Branching is what makes it a how-to,
  not a tutorial.
- **Show the expected result after every step**, however small, so the reader
  knows they are on track: "After a few moments, the server responds with…".
- **Defer explanation.** A tutorial is not the place for it. Link out to an
  explanation page instead of pausing to teach.
- **Reach the first success in the fewest steps.** Put everything else under a
  closing "Next steps". Name the promise in the title when you can honour it —
  "Run your first reasoned query in five minutes".
- **Test it on a real first-time user**, not by re-reading it yourself.

### How-to guide

- Written from the reader's goal, not the machinery's structure. Title it by the
  task: "Load an RDF dataset", "Enable OWL 2 RL reasoning".
- Action, and only action. Assume competence; link away for background.
- May branch to cover real-world variants — this is the freedom a tutorial denies
  itself.

### Reference

- **Describe, and only describe.** Neutral and factual; no opinion, no
  instruction, no teaching.
- **Mirror the product's own structure** so a reader who knows the product can
  predict where a fact lives.
- Be consistent: every entry of a kind uses the **same field order** (see
  [Reference templates](#reference-doc-templates)). Give terse examples; do not
  turn them into a lesson.

### Explanation

- Discuss the *why*: design decisions, constraints, alternatives considered, how
  pieces connect. Does not instruct and does not list every fact.
- The right home for reasoning-model background, the WCOJ-versus-hash trade-off,
  or why HornDB treats the symbolic result as the source of truth.

## Voice and tone

- **Address the reader as "you". Do not write "we".** ("You define a prefix", not
  "We define a prefix".)
- **Active voice, with the actor named.** "The reasoner materializes the closure",
  not "The closure is materialized".
- **Present tense** for how things behave. Keep "will" for genuinely future events;
  avoid the hypothetical "would".
- **Imperative mood for instructions — lead with the verb.** "Run the query", not
  "You can run the query".
- **Contractions are fine** for warmth ("it's", "you'll") — except in a negative
  warning ("Do not", not "Don't"), in reference entries, and in error-message text,
  where they read as too casual.
- **Banned words:** *please*, *simply*, *easily*, *just*, *of course*. They add no
  meaning and blame the reader when a step turns out to be hard.
- **Cut hidden-subject openers** — "there is", "there are". Name the thing.

## Oxford English (Oxford spelling)

HornDB docs use **Oxford English** — British English with **Oxford spelling**.
Oxford spelling is not the same as ordinary British spelling; the difference is
the `-ize`/`-ise` ending, and getting it right matters because a later correction
is a repo-wide sweep.

- **Use `-ize` / `-ization`** for the suffix that comes from the Greek *-izo*:
  *organize, recognize, realize, minimize, customize, emphasize, standardize,
  materialization, organization*. This is the Oxford University Press convention —
  **not** `-ise`.
- **But keep `-ise` for words where it is not that suffix** (they are not Greek
  `-izo` verbs): *analyse, paralyse, advertise, advise, arise, comprise, compromise,
  devise, exercise, improvise, revise, supervise, surprise, televise*.
- **Keep British `-our`:** *colour, behaviour, favour, neighbour.*
- **Keep British `-re`:** *centre, metre, litre, fibre, theatre.* (A `metre` is the
  unit; a `meter` is a measuring device.)
- **Noun/verb `-ce`/`-se`:** *licence*/*practice* as nouns, *license*/*practise* as
  verbs; *defence, offence.*
- **`-ogue`:** *catalogue, dialogue, analogue.*
- **Double the consonant before a suffix:** *travelled, labelled, modelling,
  cancelled, signalling.*
- Prefer *grey* over *gray*, *towards*/*forwards* with the *s*.

Reserve American spellings for anything that is a **literal identifier** — a
keyword, a function name, a config key, an error symbol, an API field. `rdf:type`,
`optimize=true`, and a `Serializer` type name are code and stay exactly as the
software spells them, even mid-sentence.

Punctuation and conventions:

- **Use the Oxford (serial) comma:** "reference, guides, and tutorials".
- **Dates:** ISO `2026-07-20` in tables and output; "20 July 2026" in prose. Never
  the ambiguous all-numeric `07/20`.
- **Units:** a space between number and unit (`4 MB`, `10 ms`), except `%` and `°`.

## Structure, headings, and lists

- **One idea per sentence. Shorter is better** — cut every word that carries no
  meaning.
- **Front-load.** Lead with what matters and put keywords first so the page is
  skimmable, like a newspaper.
- **Put the condition before the instruction:** "If the store is empty, load a
  dataset first", not the reverse.
- **Break up noun strings:** "custom settings for a project", not "project custom
  settings".
- **Headings in sentence case**, no end punctuation, descriptive and unique across
  siblings.
- **Do not skip heading levels** (H2 → H3, never H2 → H4). No links or bold inside a
  heading. If you are nesting past H4, split the page instead.
- **Lists:** numbered for a sequence, bulleted otherwise. Keep items parallel — all
  start with a verb, or all with a noun. Serial comma in run-in lists.

## Code samples

- **Introduce every sample** with a lead-in sentence saying what it does.
- **Make every sample copy-paste-correct.** A reader should be able to paste and
  run it. If a block is deliberately incomplete, do not offer click-to-copy on it.
- **Mark an omission with a real comment in the language** (`# … your prefixes
  here`), never a bare `…`.
- **Show the expected output** for anything a reader would want to verify, clearly
  separated from the input.
- **Follow the language's own formatting** — the SPARQL, Rust, Python, or shell
  each read as idiomatic to that language.
- Use **backticks** for a filename, keyword, function, config key, or short literal
  in prose; use a **fenced code block** for multi-line code or CLI sessions.
- **Lead with the example** on a concept page — readers look there first — but do
  not example *everything*, or the page stops being skimmable.

## Admonitions

Use sparingly. Stacked notices lose their force and a wall of boxes reads as
noise. Prefer plain prose; reach for a box only when the point genuinely sits
outside the flow. Three levels, in rising severity:

- **Note** — useful but not critical, and awkward to fit in the sentence.
- **Caution** — proceed with care; a wrong move here costs time.
- **Warning** — do not do this, or the action cannot be undone (data loss, an
  irreversible migration).

Never demote a prerequisite, a cross-reference, or an actual step into a note —
those belong in the body.

## Links and accessibility

- **Descriptive link text — never "click here", "this", or "here".** Screen-reader
  users jump from link to link, so the text must make sense on its own. Write "see
  the [SPARQL function reference]", not "see [here]".
- **Do not reuse the same link text** for two different targets.
- **Alt text describes the meaning, not the pixels:** "A client sending a SPARQL
  UPDATE to the HornDB server", under ~155 characters, with no "Image of" prefix.
- Every published link must resolve on `horndb.io` — no links into internal
  `docs/specs/` or `docs/plans/` (see [Scope](#scope)).

## Terminology

- **One term per concept, everywhere.** Pick "reasoner" or "engine" and stay with
  it; switching terms makes a reader wonder whether you mean two different things.
- **Define a term of art on first use**, then use it. Introduce `Z-set`, "leapfrog
  triejoin", or "materialization" in a few plain words before leaning on it.
- **Avoid translation-hostile writing:** spell out "that is" and "for example"
  rather than *i.e.* and *e.g.*; avoid an ambiguous "it"; skip idioms.
- **Correctness beats coverage.** An incorrect doc is worse than a missing one —
  if a feature is documented wrongly, treat it as broken.

## Reference-doc templates

Strong database references (PostgreSQL, DuckDB, SQLite, ClickHouse) all use a
**fixed field order, identical in every entry**, so entries are scannable and
diff-friendly. Match one of these templates.

### Function / operator entry

Order: **signature (typed) → one-line description → arguments → returns → example
with expected result → aliases → since-version.** Put the types on the signature,
not in prose. Optional arguments in `[ ]`, repetition as `[, … ]`. Join each
example to its result with a single consistent glyph (`→`).

```
STRLEN(str: string) → integer
    Return the length of a string in characters.
    Arguments:  str — the input string.
    Returns:    the character count, as an integer.
    Example:    STRLEN("café") → 4
    Since:      0.3
```

Document operators with the same template, plus one operator-precedence table for
the whole set. For an overloaded function, list every signature in the heading and
describe them in sequence.

### Configuration setting

Order: **name (type) → default with units → how units are parsed → scope / when it
can be set → allowed range or values → tuning notes.** Keep the order identical
across every setting.

### Statement / syntax

Lead with a **synopsis**, then one subsection per clause. Use EBNF conventions
consistently: `[ ]` optional, `{ }` a required choice, `|` alternatives, *italic*
for a placeholder, `monospace` for a literal keyword.

### Errors and diagnostics

Document errors as a **stable code + symbolic-name table, grouped by class**, and
tell tool authors to **match the code, not the message text** — codes are stable
across releases and unaffected by wording changes.

## Keep the docs honest

The worst documentation failure is an example that no longer works. Guard against
it by making examples **execute**, so a broken one fails the build rather than
misleading a reader.

- **Author example-bearing pages as Quarto `.qmd`.** Quarto is a Markdown
  superset: a `.qmd` page reads as normal Markdown, but its fenced code cells
  *run* when the page is rendered, and Quarto embeds the real captured output
  underneath. The published output is therefore the tested output — a query that
  errors, or whose result changed, breaks `quarto render` in CI instead of
  shipping a wrong answer. Prose-only pages can stay plain `.md`.
  - Run HornDB from a cell through the path a reader would use: a Python cell over
    the rdflib-compatible binding (`crates/python`), or a shell cell running
    `curl` against the SPARQL HTTP endpoint. Quarto captures the result and
    renders it as the example's output — you never hand-type expected output.
  - Do not paste an expected result next to an executed cell; let Quarto produce
    it. Hand-write output only in a plain `.md` page where nothing executes.
- **Generate reference from a source of truth wherever possible** so it cannot
  drift — the way Stripe generates its API reference from an OpenAPI spec and Rust
  runs its doc examples as tests. A worked example that is also a test never lies.
- **Offer a zero-install runnable path** when you can (the DuckDB and ClickHouse
  in-browser shells, the Rust Playground "Run" button). A reader who can run the
  example without setting anything up reaches success far sooner.
- **Version-correct links** matter more than single-page-versus-multi-page: a link
  a reader copies should resolve to the HornDB version they run.

## Before you publish — checklist

- [ ] The page is exactly one mode (tutorial, how-to, reference, or explanation).
- [ ] It sits in the right directory (`ref/` for reference, `guides/`
      otherwise) and is linked from that section's index.
- [ ] Second person, active voice, present tense; no *please* / *simply* / *just*.
- [ ] Oxford spelling: `-ize` suffix, British `-our`/`-re`, Oxford comma.
- [ ] Example-bearing pages are `.qmd` and `quarto render` succeeds; output is
      captured by Quarto, not hand-typed. Omissions are real in-language comments.
- [ ] Link text is descriptive; no links to internal `docs/specs` or `docs/plans`.
- [ ] One term per concept; each term of art defined on first use.

## References

- Diátaxis framework — <https://diataxis.fr/> (and `/tutorials/`, `/how-to-guides/`,
  `/reference/`, `/explanation/`, `/compass/`)
- Google developer documentation style guide — <https://developers.google.com/style>
- Microsoft Writing Style Guide — <https://learn.microsoft.com/en-us/style-guide/>
- GitLab documentation style guide — <https://docs.gitlab.com/development/documentation/styleguide/>
- Write the Docs — documentation principles — <https://www.writethedocs.org/guide/writing/docs-principles/>
- Quarto (executable `.qmd` authoring) — <https://quarto.org/docs/authoring/markdown-basics.html>
  and code execution — <https://quarto.org/docs/computations/execution-options.html>
- Reference-doc exemplars: PostgreSQL functions and error codes
  (<https://www.postgresql.org/docs/current/functions-string.html>,
  <https://www.postgresql.org/docs/current/errcodes-appendix.html>), DuckDB
  "Friendly SQL" (<https://duckdb.org/docs/lts/sql/dialect/friendly_sql>), SQLite
  "How SQLite Is Tested" (<https://sqlite.org/testing.html>), Rust doctests
  (<https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html>).
