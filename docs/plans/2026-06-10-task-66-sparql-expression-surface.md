# Task #66 — SPARQL expression surface + GRAPH lowering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining gaps of issue #66: lower and evaluate SPARQL arithmetic, `IF`, `COALESCE`, and the common builtin function surface (string / numeric / regex / type-check / datetime accessors), and lower `GRAPH` patterns, so the LDBC SPB and trainmarks query mixes translate without `UnsupportedAlgebra`.

**Architecture:** Three-layer change confined to `crates/sparql`: (1) extend the internal `Expr` IR (`src/algebra/mod.rs`) with arithmetic/`If`/`Coalesce`/`Func` variants, (2) extend `translate_expr` (`src/algebra/translate.rs`) to lower the matching `spargebra::algebra::Expression` variants, (3) extend the runtime evaluator (`src/exec/runtime.rs`) with term-level evaluation for the new variants. `GRAPH` lowers transparently to its inner pattern (Stage-1 merged-graph semantics; the graph-name variable stays unbound). No new physical-plan operator is needed.

**Context already established (verified against the code, 2026-06-10):**
- `GROUP BY` + all aggregates, `<=`/`>=`/`IN`/`NOT IN` already translate and evaluate (`translate.rs:198-212, 315-349`; `runtime.rs:138-197, 444-491`). Tests exist in `tests/exec_aggregate.rs` and `tests/exec_filter_ops.rs`.
- The remaining `UnsupportedAlgebra` sites for #66 are the `other =>` catch-all in `translate_expr` (`translate.rs:343`) and `GraphPattern::Graph` (`translate.rs:217`).
- Literals are held in N-Triples lexical form (`Term::Literal("\"4\"^^<…integer>")`). The Stage-1 `MemStore` erases term kinds on scan — bound objects arrive as `Term::Iri(raw)` — so all value extraction must go through the raw string regardless of variant (see `literal_value`/`lex` in `runtime.rs`).
- Numeric model is best-effort `f64` (`numeric_value`, `numeric_term`, `integer_literal`, `decimal_literal` in `runtime.rs:199-258`). The new arithmetic follows the same model — no XSD type lattice in Stage 1.
- `spargebra 0.4` parses everything already; this is purely a translation + evaluation gap.

**Tech Stack:** Rust 1.90, `spargebra 0.4`, new workspace dep `regex 1`.

**Worktree:** `/Users/stig/git/sunstone/horndb/.worktrees/task-66-sparql-aggregation` on branch `task-66-sparql-aggregation`. All commands below run from that directory with:

```bash
export CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target
```

**Out of scope (keep rejecting with `UnsupportedAlgebra`):** `EXISTS`/`NOT EXISTS` (needs executor access inside expression eval), `BNODE`/`RAND`/`NOW`/`UUID`/`STRUUID` (non-deterministic), `MD5`/`SHA*`, `STRLANG`/`STRDT`, `ENCODE_FOR_URI`, `TIMEZONE`/`TZ`, `IRI()`, SPARQL 1.2 `TRIPLE`/`SUBJECT`/`PREDICATE`/`OBJECT`/`ISTRIPLE`, custom functions. `Minus`/`Service`/`Reduced`/`Lateral` stay rejected too.

---

### Task 1: Add the `regex` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace root — the worktree's, i.e. `.worktrees/task-66-sparql-aggregation/Cargo.toml`)
- Modify: `crates/sparql/Cargo.toml`

- [ ] **Step 1: Add `regex` to `[workspace.dependencies]`**

In the root `Cargo.toml`, in the `[workspace.dependencies]` table, after the `spargebra = "0.4"` line, add:

```toml
regex = "1"
```

- [ ] **Step 2: Reference it from the sparql crate**

In `crates/sparql/Cargo.toml` under `[dependencies]`, add:

```toml
regex.workspace = true
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p horndb-sparql`
Expected: clean build (regex compiles as a new dep).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/sparql/Cargo.toml
git commit -m "build(sparql): add regex workspace dependency for SPARQL REGEX/REPLACE"
```

---

### Task 2: Arithmetic, IF, COALESCE — IR, translation, evaluation

**Files:**
- Modify: `crates/sparql/src/algebra/mod.rs` (extend `Expr`)
- Modify: `crates/sparql/src/algebra/translate.rs` (`translate_expr`)
- Modify: `crates/sparql/src/exec/runtime.rs` (`eval_expr`, `eval_expr_to_term`, new helpers)
- Create: `crates/sparql/tests/exec_expressions.rs`

- [ ] **Step 1: Write failing end-to-end tests**

Create `crates/sparql/tests/exec_expressions.rs`:

```rust
//! End-to-end tests for the expanded expression surface (#66):
//! arithmetic, IF, COALESCE, builtin functions.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn store_with_prices() -> MemStore {
    let mut s = MemStore::default();
    for (subj, price) in [("a", 4), ("b", 11)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{subj}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    s
}

fn rows(q: &str, s: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, s).expect("query should run") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Lexical value of a binding, ignoring term kind and literal decoration.
fn lexical(b: &horndb_sparql::exec::Bindings, var: &str) -> String {
    let t = b.get(var).unwrap_or_else(|| panic!("unbound ?{var}"));
    let raw = match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        other => panic!("unexpected term {other:?}"),
    };
    if let Some(stripped) = raw.strip_prefix('"') {
        stripped.split('"').next().unwrap().to_owned()
    } else {
        raw
    }
}

#[test]
fn bind_arithmetic_add() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?y WHERE { ?s <http://example.org/price> ?p . BIND(?p + 1 AS ?y) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "y")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "5".into()),
            ("http://example.org/b".into(), "12".into()),
        ]
    );
}

#[test]
fn filter_arithmetic_comparison() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s WHERE { ?s <http://example.org/price> ?p . FILTER(?p * 2 > 10) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/b");
}

#[test]
fn division_yields_decimal_and_div_by_zero_drops_row() {
    let s = store_with_prices();
    // 4 / 2 = 2 ; 11 / 2 = 5.5 — both rows keep a bound ?h.
    let got = rows(
        "SELECT ?h WHERE { ?s <http://example.org/price> ?p . BIND(?p / 2 AS ?h) }",
        &s,
    );
    let mut vals: Vec<String> = got.iter().map(|b| lexical(b, "h")).collect();
    vals.sort();
    assert_eq!(vals, vec!["2".to_string(), "5.5".to_string()]);
    // Division by zero is an expression error: BIND leaves ?z unbound.
    let got = rows(
        "SELECT ?s ?z WHERE { ?s <http://example.org/price> ?p . BIND(?p / 0 AS ?z) }",
        &s,
    );
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|b| b.get("z").is_none()));
}

#[test]
fn unary_minus() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?n WHERE { ?s <http://example.org/price> ?p . BIND(-?p AS ?n) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "n"), "-4");
}

#[test]
fn if_in_bind() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?label WHERE { ?s <http://example.org/price> ?p . \
         BIND(IF(?p > 10, \"expensive\", \"cheap\") AS ?label) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "label")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "cheap".into()),
            ("http://example.org/b".into(), "expensive".into()),
        ]
    );
}

#[test]
fn coalesce_picks_first_bound() {
    let s = store_with_prices();
    // ?unbound never binds; COALESCE falls through to ?p.
    let got = rows(
        "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
         OPTIONAL { ?s <http://example.org/missing> ?unbound } \
         BIND(COALESCE(?unbound, ?p) AS ?v) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "v"), "4");
}

#[test]
fn sum_of_products_aggregate() {
    let mut s = MemStore::default();
    for (o, qty, price) in [("o1", 2, 3), ("o2", 5, 4)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/qty".into()),
            Term::Literal(format!("\"{qty}\"^^<{XSD_INT}>")),
        );
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    let got = rows(
        "SELECT (SUM(?q * ?p) AS ?total) WHERE { \
         ?o <http://example.org/qty> ?q . ?o <http://example.org/price> ?p }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "total"), "26");
}
```

- [ ] **Step 2: Run to verify the tests fail for the right reason**

Run: `cargo test -p horndb-sparql --test exec_expressions`
Expected: every test panics with `UnsupportedAlgebra("expression: …")` (from `execute_query`), NOT with compile errors.

- [ ] **Step 3: Extend the `Expr` IR**

In `crates/sparql/src/algebra/mod.rs`, replace the `Expr` doc comment and add variants at the end of the enum (after `In`):

```rust
/// A SPARQL expression. Stage 1 covers comparisons, boolean
/// connectives, arithmetic, `IF`, `COALESCE`, and the common builtin
/// functions ([`Func`]); evaluation is best-effort over the lexical
/// forms (see `exec::runtime`). `EXISTS`, non-deterministic builtins
/// (`RAND`, `NOW`, `UUID`, …) and custom functions are out of scope
/// and rejected at translation time.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Term(Term),
    Eq(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    Le(Box<Expr>, Box<Expr>),
    Ge(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Bound(Var),
    /// `expr IN (a, b, …)` — true when `expr` equals any list element.
    /// `NOT IN` is lowered by spargebra as `Not(In(...))`.
    In(Box<Expr>, Vec<Expr>),
    /// Numeric arithmetic over the Stage-1 best-effort f64 model.
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    /// `IF(cond, then, else)`.
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `COALESCE(e1, e2, …)` — first argument that evaluates without
    /// error to a bound term.
    Coalesce(Vec<Expr>),
    /// A builtin function call, e.g. `STRLEN(?x)` or `REGEX(?x, "p", "i")`.
    Func(Func, Vec<Expr>),
}

/// Builtin functions evaluated in Stage 1. Argument arity is checked
/// at evaluation time; wrong arity is an expression error (unbound
/// result), matching the general best-effort error model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    // Strings
    Str,
    Lang,
    LangMatches,
    Datatype,
    StrLen,
    SubStr,
    UCase,
    LCase,
    StrStarts,
    StrEnds,
    Contains,
    StrBefore,
    StrAfter,
    Concat,
    Replace,
    Regex,
    // Numerics
    Abs,
    Ceil,
    Floor,
    Round,
    // Term type checks
    IsIri,
    IsBlank,
    IsLiteral,
    IsNumeric,
    // xsd:dateTime accessors
    Year,
    Month,
    Day,
    Hours,
    Minutes,
    Seconds,
}
```

(Keep the existing variants exactly as they are — the listing above shows the full enum for clarity; `Func` is a new enum after `Expr`.)

Note `Func` derives `Copy` — it is field-free.

- [ ] **Step 4: Translate the new spargebra variants**

In `crates/sparql/src/algebra/translate.rs`:

1. Extend the import at the top of the file: `use crate::algebra::{AggFunc, Aggregate, Algebra, Expr, Func, OrderDir, Term, TriplePattern, Var};`
2. Also import `Function`: change the `spargebra::algebra::{…}` import to include `Function`.
3. In `translate_expr`, before the `other =>` catch-all, add:

```rust
        E::Add(a, b) => Expr::Add(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Subtract(a, b) => {
            Expr::Sub(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::Multiply(a, b) => {
            Expr::Mul(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::Divide(a, b) => Expr::Div(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::UnaryPlus(a) => translate_expr(a)?,
        E::UnaryMinus(a) => Expr::Neg(Box::new(translate_expr(a)?)),
        E::If(c, t, f) => Expr::If(
            Box::new(translate_expr(c)?),
            Box::new(translate_expr(t)?),
            Box::new(translate_expr(f)?),
        ),
        E::Coalesce(args) => {
            Expr::Coalesce(args.iter().map(translate_expr).collect::<Result<Vec<_>>>()?)
        }
        E::FunctionCall(func, args) => {
            let f = translate_function(func)?;
            Expr::Func(
                f,
                args.iter().map(translate_expr).collect::<Result<Vec<_>>>()?,
            )
        }
```

4. Add the function mapper next to `translate_expr`:

```rust
/// Map a spargebra builtin to the Stage-1 [`Func`] set. Functions
/// outside the set (non-deterministic, hashing, SPARQL 1.2 triple
/// accessors, custom IRIs) are rejected here so the planner and
/// runtime never see them.
fn translate_function(f: &Function) -> Result<Func> {
    Ok(match f {
        Function::Str => Func::Str,
        Function::Lang => Func::Lang,
        Function::LangMatches => Func::LangMatches,
        Function::Datatype => Func::Datatype,
        Function::StrLen => Func::StrLen,
        Function::SubStr => Func::SubStr,
        Function::UCase => Func::UCase,
        Function::LCase => Func::LCase,
        Function::StrStarts => Func::StrStarts,
        Function::StrEnds => Func::StrEnds,
        Function::Contains => Func::Contains,
        Function::StrBefore => Func::StrBefore,
        Function::StrAfter => Func::StrAfter,
        Function::Concat => Func::Concat,
        Function::Replace => Func::Replace,
        Function::Regex => Func::Regex,
        Function::Abs => Func::Abs,
        Function::Ceil => Func::Ceil,
        Function::Floor => Func::Floor,
        Function::Round => Func::Round,
        Function::IsIri => Func::IsIri,
        Function::IsBlank => Func::IsBlank,
        Function::IsLiteral => Func::IsLiteral,
        Function::IsNumeric => Func::IsNumeric,
        Function::Year => Func::Year,
        Function::Month => Func::Month,
        Function::Day => Func::Day,
        Function::Hours => Func::Hours,
        Function::Minutes => Func::Minutes,
        Function::Seconds => Func::Seconds,
        other => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "function: {other:?}"
            )));
        }
    })
}
```

If a variant name in this list does not exist in `spargebra 0.4` (compiler error), check the real name with `cargo doc -p spargebra --no-deps` or the `.cargo/registry` source and adjust — do not drop the function.

- [ ] **Step 5: Evaluate the new variants in the runtime**

In `crates/sparql/src/exec/runtime.rs`:

1. Extend the import: `use crate::algebra::{AggFunc, Aggregate, Expr, Func, OrderDir, Term, Var};`

2. Add helpers after `numeric_value` (around line 258):

```rust
/// SPARQL effective boolean value, best effort over lexical forms:
/// "true"/"false" literals, numeric ≠ 0, otherwise non-empty string.
fn ebv(t: &Term) -> bool {
    let v = literal_value(t);
    match v.as_str() {
        "true" => true,
        "false" => false,
        _ => match v.trim().parse::<f64>() {
            Ok(n) => n != 0.0,
            Err(_) => !v.is_empty(),
        },
    }
}

/// Wrap a lexical value as a plain (unquoted-form) literal term,
/// escaping interior quotes, matching the GroupConcat output style.
fn plain_literal(s: &str) -> Term {
    Term::Literal(format!("\"{}\"", s.replace('"', "\\\"")))
}

/// Binary arithmetic over the Stage-1 f64 model. `None` (expression
/// error) when either side is non-numeric or on division by zero.
fn arith(op: fn(f64, f64) -> f64, a: Option<f64>, b: Option<f64>) -> Option<Term> {
    Some(numeric_term(op(a?, b?)))
}
```

3. In `eval_expr` (the `-> Result<bool>` function), add arms before the final `Expr::Term(t)` arm:

```rust
        Expr::Add(..)
        | Expr::Sub(..)
        | Expr::Mul(..)
        | Expr::Div(..)
        | Expr::Neg(..)
        | Expr::If(..)
        | Expr::Coalesce(..)
        | Expr::Func(..) => match eval_expr_to_term(e, b)? {
            Some(t) => ebv(&t),
            None => false,
        },
```

4. In `eval_expr_to_term`, add arms after the boolean catch-all arm:

```rust
        Expr::Add(x, y) => arith(
            |a, b| a + b,
            eval_expr_to_term(x, b)?.as_ref().and_then(numeric_value),
            eval_expr_to_term(y, b)?.as_ref().and_then(numeric_value),
        ),
        Expr::Sub(x, y) => arith(
            |a, b| a - b,
            eval_expr_to_term(x, b)?.as_ref().and_then(numeric_value),
            eval_expr_to_term(y, b)?.as_ref().and_then(numeric_value),
        ),
        Expr::Mul(x, y) => arith(
            |a, b| a * b,
            eval_expr_to_term(x, b)?.as_ref().and_then(numeric_value),
            eval_expr_to_term(y, b)?.as_ref().and_then(numeric_value),
        ),
        Expr::Div(x, y) => {
            let d = eval_expr_to_term(y, b)?.as_ref().and_then(numeric_value);
            match d {
                Some(d) if d != 0.0 => arith(
                    |a, b| a / b,
                    eval_expr_to_term(x, b)?.as_ref().and_then(numeric_value),
                    Some(d),
                ),
                _ => None, // division by zero / non-numeric divisor
            }
        }
        Expr::Neg(x) => eval_expr_to_term(x, b)?
            .as_ref()
            .and_then(numeric_value)
            .map(|n| numeric_term(-n)),
        Expr::If(c, t, f) => {
            if eval_expr(c, b)? {
                eval_expr_to_term(t, b)?
            } else {
                eval_expr_to_term(f, b)?
            }
        }
        Expr::Coalesce(args) => {
            let mut found = None;
            for a in args {
                if let Some(t) = eval_expr_to_term(a, b)? {
                    found = Some(t);
                    break;
                }
            }
            found
        }
        Expr::Func(f, args) => eval_func(*f, args, b)?,
```

5. Add a stub `eval_func` (filled in Task 3) near the bottom of the eval helpers:

```rust
/// Evaluate a builtin function call. `Ok(None)` is "expression error"
/// (the SPARQL error value): the binding stays unbound / the filter
/// row drops.
fn eval_func(f: Func, args: &[Expr], b: &Bindings) -> Result<Option<Term>> {
    let _ = (f, args, b);
    Ok(None)
}
```

(`numeric_value` takes `&Term`; the `.as_ref().and_then(numeric_value)` chains above rely on that — if the signature differs, adapt at the call sites, not by changing `numeric_value`.)

- [ ] **Step 6: Run the Task-2 tests**

Run: `cargo test -p horndb-sparql --test exec_expressions`
Expected: all Task-2 tests PASS (`bind_arithmetic_add`, `filter_arithmetic_comparison`, `division_yields_decimal_and_div_by_zero_drops_row`, `unary_minus`, `if_in_bind`, `coalesce_picks_first_bound`, `sum_of_products_aggregate`).

Also run: `cargo test -p horndb-sparql` — no regressions in the other suites.

- [ ] **Step 7: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p horndb-sparql --all-targets -- -D warnings
git add crates/sparql
git commit -m "feat(sparql): arithmetic, IF and COALESCE expressions (#66)"
```

---

### Task 3: Builtin function surface

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (`eval_func` + literal helpers)
- Modify: `crates/sparql/tests/exec_expressions.rs` (append tests)

- [ ] **Step 1: Append failing tests**

Append to `crates/sparql/tests/exec_expressions.rs`:

```rust
fn store_with_names() -> MemStore {
    let mut s = MemStore::default();
    let data = [
        ("a", "\"Alice\"@en"),
        ("b", "\"bob\""),
        ("c", "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
    ];
    for (subj, lit) in data {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{subj}")),
            Term::Iri("http://example.org/name".into()),
            Term::Literal(lit.to_owned()),
        );
    }
    s
}

#[test]
fn string_functions() {
    let s = store_with_names();
    let q = "SELECT ?s ?len ?up WHERE { ?s <http://example.org/name> ?n . \
             BIND(STRLEN(?n) AS ?len) BIND(UCASE(?n) AS ?up) \
             FILTER(?s = <http://example.org/b>) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "len"), "3");
    assert_eq!(lexical(&got[0], "up"), "BOB");
}

#[test]
fn substr_and_concat() {
    let s = store_with_names();
    let q = "SELECT ?x WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(CONCAT(SUBSTR(?n, 1, 2), \"!\") AS ?x) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "x"), "Al!");
}

#[test]
fn str_starts_ends_contains_before_after() {
    let s = store_with_names();
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(STRSTARTS(?n, \"Al\") && STRENDS(?n, \"ce\") && CONTAINS(?n, \"lic\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");

    let q = "SELECT ?b ?a WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(STRBEFORE(?n, \"i\") AS ?b) BIND(STRAFTER(?n, \"i\") AS ?a) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "b"), "Al");
    assert_eq!(lexical(&got[0], "a"), "ce");
}

#[test]
fn regex_and_replace() {
    let s = store_with_names();
    // Case-insensitive match.
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . FILTER(REGEX(?n, \"^ali\", \"i\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");
    // Invalid pattern is an expression error: filter drops all rows.
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . FILTER(REGEX(?n, \"(\")) }";
    assert_eq!(rows(q, &s).len(), 0);
    // REPLACE with a capture group.
    let q = "SELECT ?x WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/b>) \
             BIND(REPLACE(?n, \"b(o)\", \"B$1\") AS ?x) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "x"), "Bob");
}

#[test]
fn str_lang_datatype() {
    let s = store_with_names();
    let q = "SELECT ?lang ?dt WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(LANG(?n) AS ?lang) BIND(DATATYPE(?n) AS ?dt) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "lang"), "en");
    assert_eq!(
        lexical(&got[0], "dt"),
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
    );
    let q = "SELECT ?dt WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/c>) BIND(DATATYPE(?n) AS ?dt) }";
    let got = rows(q, &s);
    assert_eq!(
        lexical(&got[0], "dt"),
        "http://www.w3.org/2001/XMLSchema#integer"
    );
    // LANGMATCHES
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(LANGMATCHES(LANG(?n), \"en\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");
}

#[test]
fn numeric_functions() {
    let s = store_with_prices();
    let q = "SELECT ?abs ?ceil ?floor ?round WHERE { \
             <http://example.org/a> <http://example.org/price> ?p . \
             BIND(ABS(0 - ?p) AS ?abs) BIND(CEIL(?p / 2) AS ?ceil) \
             BIND(FLOOR(?p / 2) AS ?floor) BIND(ROUND(?p / 2) AS ?round) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "abs"), "4"); // |0-4| = 4
    assert_eq!(lexical(&got[0], "ceil"), "2"); // ceil(2.0)
    assert_eq!(lexical(&got[0], "floor"), "2"); // floor(2.0)
    assert_eq!(lexical(&got[0], "round"), "2"); // round(2.0)
}

#[test]
fn type_check_functions() {
    let s = store_with_names();
    // isLITERAL on a literal-valued object; isIRI on the subject.
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(ISLITERAL(?n) && ISIRI(?s) && !ISBLANK(?s)) \
             FILTER(?s = <http://example.org/c>) FILTER(ISNUMERIC(?n)) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/c");
}

#[test]
fn datetime_accessors() {
    let mut s = MemStore::default();
    s.insert_triple(
        Term::Iri("http://example.org/e".into()),
        Term::Iri("http://example.org/at".into()),
        Term::Literal(
            "\"2026-06-10T12:34:56\"^^<http://www.w3.org/2001/XMLSchema#dateTime>".into(),
        ),
    );
    let q = "SELECT ?y ?mo ?d ?h ?mi ?sec WHERE { ?e <http://example.org/at> ?t . \
             BIND(YEAR(?t) AS ?y) BIND(MONTH(?t) AS ?mo) BIND(DAY(?t) AS ?d) \
             BIND(HOURS(?t) AS ?h) BIND(MINUTES(?t) AS ?mi) BIND(SECONDS(?t) AS ?sec) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "y"), "2026");
    assert_eq!(lexical(&got[0], "mo"), "6");
    assert_eq!(lexical(&got[0], "d"), "10");
    assert_eq!(lexical(&got[0], "h"), "12");
    assert_eq!(lexical(&got[0], "mi"), "34");
    assert_eq!(lexical(&got[0], "sec"), "56");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p horndb-sparql --test exec_expressions`
Expected: the new tests fail (functions evaluate to `None` via the stub → unbound bindings / empty filters); Task-2 tests still pass.

- [ ] **Step 3: Implement `eval_func` and literal helpers**

In `crates/sparql/src/exec/runtime.rs`, add after `literal_lexical`:

```rust
/// Split an N-Triples literal raw form into (lexical, lang, datatype).
/// Non-literal raw forms (no leading quote) yield (raw, None, None).
fn literal_parts(raw: &str) -> (String, Option<String>, Option<String>) {
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return (raw.to_owned(), None, None);
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            let value = raw[1..i].to_owned();
            let tail = &raw[i + 1..];
            if let Some(lang) = tail.strip_prefix('@') {
                return (value, Some(lang.to_owned()), None);
            }
            if let Some(dt) = tail.strip_prefix("^^") {
                let dt = dt.trim_start_matches('<').trim_end_matches('>');
                return (value, None, Some(dt.to_owned()));
            }
            return (value, None, None);
        }
        i += 1;
    }
    (raw.to_owned(), None, None)
}

/// Best-effort term-kind classification on the raw lexical form. The
/// Stage-1 `MemStore` erases kinds on scan, so this looks at the string
/// shape rather than the enum variant.
fn term_kind(t: &Term) -> TermKind {
    match t {
        Term::Literal(_) => TermKind::Literal,
        Term::BlankNode(_) => TermKind::Blank,
        Term::Iri(s) => {
            if s.starts_with('"') {
                TermKind::Literal
            } else if s.starts_with("_:") {
                TermKind::Blank
            } else {
                TermKind::Iri
            }
        }
        Term::Var(_) | Term::Triple(_) => TermKind::Other,
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum TermKind {
    Iri,
    Blank,
    Literal,
    Other,
}

/// Compile a SPARQL REGEX/REPLACE pattern with its flags string.
/// Unsupported flag characters or an invalid pattern yield `None`
/// (expression error).
fn compile_regex(pattern: &str, flags: &str) -> Option<regex::Regex> {
    let mut b = regex::RegexBuilder::new(pattern);
    for f in flags.chars() {
        match f {
            'i' => {
                b.case_insensitive(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            'm' => {
                b.multi_line(true);
            }
            'x' => {
                b.ignore_whitespace(true);
            }
            _ => return None,
        }
    }
    b.build().ok()
}
```

Replace the stub `eval_func` with:

```rust
/// Evaluate a builtin function call. `Ok(None)` is "expression error"
/// (the SPARQL error value): the binding stays unbound / the filter
/// row drops. All value extraction goes through the raw lexical form
/// because the Stage-1 `MemStore` erases term kinds on scan.
fn eval_func(f: Func, args: &[Expr], b: &Bindings) -> Result<Option<Term>> {
    // Evaluate one argument to a term; `None` short-circuits the call.
    let term = |i: usize| -> Result<Option<Term>> {
        match args.get(i) {
            Some(e) => eval_expr_to_term(e, b),
            None => Ok(None),
        }
    };
    // The argument's plain string value (literal lexical form).
    let s = |i: usize| -> Result<Option<String>> { Ok(term(i)?.as_ref().map(literal_value)) };
    // The argument as a number.
    let num =
        |i: usize| -> Result<Option<f64>> { Ok(term(i)?.as_ref().and_then(numeric_value)) };
    let bool_lit = |v: bool| Some(Term::Literal(if v { "true" } else { "false" }.into()));

    Ok(match f {
        Func::Str => term(0)?.map(|t| plain_literal(&literal_value(&t))),
        Func::Lang => term(0)?.map(|t| {
            let (_, lang, _) = literal_parts(&lex(&t));
            plain_literal(&lang.unwrap_or_default())
        }),
        Func::LangMatches => match (s(0)?, s(1)?) {
            (Some(tag), Some(range)) => {
                let tag = tag.to_ascii_lowercase();
                let range = range.to_ascii_lowercase();
                let ok = if range == "*" {
                    !tag.is_empty()
                } else {
                    tag == range || tag.starts_with(&format!("{range}-"))
                };
                bool_lit(ok)
            }
            _ => None,
        },
        Func::Datatype => term(0)?.and_then(|t| {
            if term_kind(&t) != TermKind::Literal {
                return None;
            }
            let (_, lang, dt) = literal_parts(&lex(&t));
            let iri = if let Some(dt) = dt {
                dt
            } else if lang.is_some() {
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".to_owned()
            } else {
                "http://www.w3.org/2001/XMLSchema#string".to_owned()
            };
            Some(Term::Iri(iri))
        }),
        Func::StrLen => s(0)?.map(|v| integer_literal(v.chars().count() as i64)),
        Func::SubStr => {
            let (text, start) = match (s(0)?, num(1)?) {
                (Some(t), Some(s)) => (t, s),
                _ => return Ok(None),
            };
            // SPARQL SUBSTR is 1-based; len is optional (to end).
            let start = (start.round() as i64 - 1).max(0) as usize;
            let chars: Vec<char> = text.chars().collect();
            let taken: String = match args.len() {
                2 => chars.iter().skip(start).collect(),
                3 => match num(2)? {
                    Some(l) => chars
                        .iter()
                        .skip(start)
                        .take(l.round().max(0.0) as usize)
                        .collect(),
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
            Some(plain_literal(&taken))
        }
        Func::UCase => s(0)?.map(|v| plain_literal(&v.to_uppercase())),
        Func::LCase => s(0)?.map(|v| plain_literal(&v.to_lowercase())),
        Func::StrStarts => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.starts_with(&b)),
            _ => None,
        },
        Func::StrEnds => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.ends_with(&b)),
            _ => None,
        },
        Func::Contains => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.contains(&b)),
            _ => None,
        },
        Func::StrBefore => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => Some(plain_literal(
                a.find(&b).map(|i| &a[..i]).unwrap_or_default(),
            )),
            _ => None,
        },
        Func::StrAfter => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => Some(plain_literal(
                a.find(&b).map(|i| &a[i + b.len()..]).unwrap_or_default(),
            )),
            _ => None,
        },
        Func::Concat => {
            let mut out = String::new();
            for (i, _) in args.iter().enumerate() {
                match s(i)? {
                    Some(v) => out.push_str(&v),
                    None => return Ok(None),
                }
            }
            Some(plain_literal(&out))
        }
        Func::Replace => {
            let (text, pat, repl) = match (s(0)?, s(1)?, s(2)?) {
                (Some(t), Some(p), Some(r)) => (t, p, r),
                _ => return Ok(None),
            };
            let flags = if args.len() == 4 {
                match s(3)? {
                    Some(f) => f,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            compile_regex(&pat, &flags)
                .map(|re| plain_literal(&re.replace_all(&text, repl.as_str())))
        }
        Func::Regex => {
            let (text, pat) = match (s(0)?, s(1)?) {
                (Some(t), Some(p)) => (t, p),
                _ => return Ok(None),
            };
            let flags = if args.len() == 3 {
                match s(2)? {
                    Some(f) => f,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            compile_regex(&pat, &flags).and_then(|re| bool_lit(re.is_match(&text)))
        }
        Func::Abs => num(0)?.map(|n| numeric_term(n.abs())),
        Func::Ceil => num(0)?.map(|n| numeric_term(n.ceil())),
        Func::Floor => num(0)?.map(|n| numeric_term(n.floor())),
        Func::Round => num(0)?.map(|n| numeric_term(n.round())),
        Func::IsIri => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Iri)),
        Func::IsBlank => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Blank)),
        Func::IsLiteral => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Literal)),
        Func::IsNumeric => term(0)?
            .as_ref()
            .and_then(|t| bool_lit(numeric_value(t).is_some())),
        Func::Year | Func::Month | Func::Day | Func::Hours | Func::Minutes | Func::Seconds => {
            let v = match s(0)? {
                Some(v) => v,
                None => return Ok(None),
            };
            if datetime_key(&v).is_none() {
                return Ok(None);
            }
            // Validated shape: YYYY-MM-DDThh:mm:ss(.fff…)?
            let field = |a: usize, z: usize| v[a..z].parse::<i64>().ok();
            match f {
                Func::Year => field(0, 4).map(integer_literal),
                Func::Month => field(5, 7).map(integer_literal),
                Func::Day => field(8, 10).map(integer_literal),
                Func::Hours => field(11, 13).map(integer_literal),
                Func::Minutes => field(14, 16).map(integer_literal),
                _ => {
                    // SECONDS — keep any fractional part.
                    let tail: String = v[17..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    tail.parse::<f64>().ok().map(numeric_term)
                }
            }
        }
    })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test exec_expressions`
Expected: ALL tests pass (Task 2 + Task 3).

Run: `cargo test -p horndb-sparql`
Expected: full crate green.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p horndb-sparql --all-targets -- -D warnings
git add crates/sparql
git commit -m "feat(sparql): builtin function surface — string/regex/numeric/type/datetime (#66)"
```

---

### Task 4: GRAPH pattern lowering

**Files:**
- Modify: `crates/sparql/src/algebra/translate.rs` (`translate_pattern` Graph arm)
- Modify: `crates/sparql/tests/exec_expressions.rs` (append tests)
- Modify: `crates/sparql/INTEGRATION-NOTES.md` (document the semantics)

- [ ] **Step 1: Append failing tests**

Append to `crates/sparql/tests/exec_expressions.rs`:

```rust
#[test]
fn graph_iri_lowers_to_inner_pattern() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s WHERE { GRAPH <http://example.org/g> { ?s <http://example.org/price> ?p } }",
        &s,
    );
    // Stage-1 merged-graph semantics: GRAPH is transparent.
    assert_eq!(got.len(), 2);
}

#[test]
fn graph_var_lowers_with_unbound_graph_var() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?g ?s WHERE { GRAPH ?g { ?s <http://example.org/price> ?p } }",
        &s,
    );
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|b| b.get("g").is_none()));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p horndb-sparql --test exec_expressions graph_`
Expected: both fail with `UnsupportedAlgebra("Graph")`.

- [ ] **Step 3: Lower GRAPH transparently**

In `crates/sparql/src/algebra/translate.rs`, replace the line

```rust
        GraphPattern::Graph { .. } => Err(SparqlError::UnsupportedAlgebra("Graph".into())),
```

with:

```rust
        // Stage-1 merged-graph semantics: the executor holds a single
        // graph (SPB/W3C corpora are loaded from flat dumps), so
        // `GRAPH <iri> { P }` and `GRAPH ?g { P }` lower to `P`. A
        // graph-name variable stays unbound in the results. True
        // named-graph scoping arrives with the storage wiring (#67) —
        // see INTEGRATION-NOTES.md.
        GraphPattern::Graph { name: _, inner } => translate_pattern(inner, cfg),
```

- [ ] **Step 4: Document in INTEGRATION-NOTES.md**

Append to `crates/sparql/INTEGRATION-NOTES.md`:

```markdown
## GRAPH patterns (Stage 1, #66)

`GRAPH <iri> { P }` and `GRAPH ?g { P }` lower transparently to `P`.
The Stage-1 executor holds a single merged graph (corpora are loaded
from flat triple dumps), so there is no named-graph store to scope
against; a graph-name variable remains unbound in results. This makes
the SPB named-graph queries (Q10/Q12) translate and run. Correct
named-graph scoping (zero solutions for absent graphs, `?g` binding
per named graph) is deliberately deferred to the storage wiring
increment (#67), where quads exist.
```

- [ ] **Step 5: Run tests, fmt, clippy, commit**

Run: `cargo test -p horndb-sparql --test exec_expressions`
Expected: all pass.

```bash
cargo fmt --all
cargo clippy -p horndb-sparql --all-targets -- -D warnings
git add crates/sparql
git commit -m "feat(sparql): lower GRAPH patterns with Stage-1 merged-graph semantics (#66)"
```

---

### Task 5: Grow the W3C-style harness subset

**Files:**
- Create: `crates/harness/tests/fixtures/sparql11/selected_subset/expr-001/{data.nt,query.rq,expected.srj,form}`
- Create: `crates/harness/tests/fixtures/sparql11/selected_subset/expr-002/{data.nt,query.rq,expected.srj,form}`
- Modify: `harness/selected.toml` (`[sparql_query]`)
- Modify: `crates/sparql/tests/w3c_suite.rs` (two `w3c_case!` lines)

- [ ] **Step 1: Create fixture `expr-001` (BIND + IF + arithmetic)**

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-001/data.nt`:

```
<http://example.org/a> <http://example.org/price> "4"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/b> <http://example.org/price> "11"^^<http://www.w3.org/2001/XMLSchema#integer> .
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-001/query.rq`:

```
SELECT ?s ?label WHERE {
  ?s <http://example.org/price> ?p .
  BIND(IF(?p * 2 > 20, "expensive", "cheap") AS ?label)
}
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-001/expected.srj`:

```json
{
  "head": { "vars": ["s", "label"] },
  "results": {
    "bindings": [
      { "s": { "type": "uri", "value": "http://example.org/a" },
        "label": { "type": "literal", "value": "cheap" } },
      { "s": { "type": "uri", "value": "http://example.org/b" },
        "label": { "type": "literal", "value": "expensive" } }
    ]
  }
}
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-001/form`:

```
select
```

- [ ] **Step 2: Create fixture `expr-002` (GROUP BY + SUM over arithmetic)**

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-002/data.nt`:

```
<http://example.org/o1> <http://example.org/qty> "2"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/o1> <http://example.org/price> "3"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/o2> <http://example.org/qty> "5"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/o2> <http://example.org/price> "4"^^<http://www.w3.org/2001/XMLSchema#integer> .
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-002/query.rq`:

```
SELECT (SUM(?q * ?p) AS ?total) WHERE {
  ?o <http://example.org/qty> ?q .
  ?o <http://example.org/price> ?p .
}
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-002/expected.srj`:

```json
{
  "head": { "vars": ["total"] },
  "results": {
    "bindings": [
      { "total": { "type": "literal", "value": "26",
                   "datatype": "http://www.w3.org/2001/XMLSchema#integer" } }
    ]
  }
}
```

`crates/harness/tests/fixtures/sparql11/selected_subset/expr-002/form`:

```
select
```

- [ ] **Step 3: Register the fixtures**

In `harness/selected.toml`, extend the `[sparql_query]` list:

```toml
[sparql_query]
tests = [
    "selected_subset/basic-001",
    "selected_subset/basic-002",
    "selected_subset/basic-003",
    "selected_subset/basic-004",
    "selected_subset/basic-005",
    "selected_subset/expr-001",
    "selected_subset/expr-002",
]
```

In `crates/sparql/tests/w3c_suite.rs`, after the `basic_005` line add:

```rust
w3c_case!(expr_001, "expr-001");
w3c_case!(expr_002, "expr-002");
```

- [ ] **Step 4: Run the suite**

Run: `cargo test -p horndb-sparql --test w3c_suite`
Expected: 7 tests pass. If `expr_002` fails on the expected JSON, inspect the actual output in the assertion diff — the multiset comparison prints both sides — and fix the ROOT CAUSE (do not just edit expected.srj to whatever the engine emitted unless the emitted form is genuinely correct per the serializer's documented behaviour).

- [ ] **Step 5: Commit**

```bash
git add crates/harness/tests/fixtures/sparql11/selected_subset/expr-001 \
        crates/harness/tests/fixtures/sparql11/selected_subset/expr-002 \
        harness/selected.toml crates/sparql/tests/w3c_suite.rs
git commit -m "test(sparql): grow W3C-style subset with expression + aggregation cases (#66)"
```

---

### Task 6: Docs sync + full workspace verification

**Files:**
- Modify: `docs/architecture.md` (SPEC-07 row)
- (Do NOT touch `TASKS.md` — its transition is a locked commit on `main` after the merge.)

- [ ] **Step 1: Update `docs/architecture.md`**

Find the SPEC-07 SPARQL section/row that describes the expression/aggregation gap (search for `aggregation` / `GROUP BY` / `#66`). Update the **Status** wording so it reflects: aggregation + expanded expression surface (arithmetic, `IF`, `COALESCE`, string/regex/numeric/type/datetime builtins) — **implemented**; `GRAPH` lowers with Stage-1 merged-graph semantics (true named-graph scoping deferred to #67). Keep the format of the surrounding rows; do not restructure the document.

- [ ] **Step 2: Full workspace verification (real output, no green-claiming)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p horndb-sparql --features server
```

Expected: all green. Read the output; if anything is red, fix it before the commit.

- [ ] **Step 3: Commit**

```bash
git add docs/architecture.md
git commit -m "docs: architecture status for SPARQL expression surface + GRAPH lowering (#66)"
```

---

## Self-review notes

- Spec coverage: #66's remaining acceptance items are (a) expanded FILTER/BIND expressions → Tasks 2–3, (b) GRAPH lowers without `UnsupportedAlgebra` → Task 4, (c) harness-first growth → Task 5. Aggregation itself was already delivered; Task 5's expr-002 pins it in the gated subset.
- The `spargebra` variant names in Task 2 (`Add`/`Subtract`/`Multiply`/`Divide`/`UnaryPlus`/`UnaryMinus`/`If`/`Coalesce`/`FunctionCall`) and the `Function` names are from spargebra 0.4; if the compiler disagrees on a name, fix the name, not the scope.
- Type consistency: `Func` is `Copy`; `eval_func(*f, args, b)` passes by value. `numeric_value(&Term) -> Option<f64>`, `numeric_term(f64) -> Term`, `integer_literal(i64) -> Term`, `plain_literal(&str) -> Term`, `lex(&Term) -> String` are the helper signatures used throughout.
- The runtime currently renders boolean results as bare `true`/`false` literals (`Term::Literal("true")` — unquoted); `bool_lit` matches that existing convention.
