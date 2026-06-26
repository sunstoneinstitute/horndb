//! Iterator-style runtime over [`PhysicalPlan`]. Each plan node
//! yields a `Vec<Bindings>` because Stage 1 materialises per-node;
//! true streaming is a Future Work item.

use crate::algebra::{AggFunc, Aggregate, Expr, Func, OrderDir, Term, Var};
use crate::error::{Result, SparqlError};
use crate::exec::{Bindings, Executor};
use crate::plan::PhysicalPlan;
use std::collections::{BTreeMap, HashMap};

pub struct Runtime<'a, E: Executor + ?Sized> {
    exec: &'a E,
}

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    pub fn new(exec: &'a E) -> Self {
        Self { exec }
    }

    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let v = self.eval(plan)?;
        Ok(v.into_iter())
    }

    fn eval(&self, plan: &PhysicalPlan) -> Result<Vec<Bindings>> {
        match plan {
            PhysicalPlan::BgpScan { patterns } => Ok(self.exec.scan_bgp(patterns)?.collect()),
            PhysicalPlan::Join { left, right } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                let mut out = Vec::new();
                for a in &l {
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            out.push(m);
                        }
                    }
                }
                Ok(out)
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                hash_left_join(l, r, expr.as_ref())
            }
            PhysicalPlan::Filter { expr, inner } => {
                let v = self.eval(inner)?;
                v.into_iter()
                    .map(|b| eval_expr(expr, &b).map(|keep| (b, keep)))
                    .collect::<Result<Vec<_>>>()
                    .map(|pairs| {
                        pairs
                            .into_iter()
                            .filter(|(_, k)| *k)
                            .map(|(b, _)| b)
                            .collect()
                    })
            }
            PhysicalPlan::Union { left, right } => {
                let mut a = self.eval(left)?;
                let b = self.eval(right)?;
                a.extend(b);
                Ok(a)
            }
            PhysicalPlan::Project { vars, inner } => {
                let v = self.eval(inner)?;
                Ok(v.into_iter().map(|b| project(&b, vars)).collect())
            }
            PhysicalPlan::Distinct { inner } => {
                let v = self.eval(inner)?;
                let mut seen: Vec<Bindings> = Vec::new();
                for b in v {
                    if !seen.contains(&b) {
                        seen.push(b);
                    }
                }
                Ok(seen)
            }
            PhysicalPlan::Slice {
                inner,
                start,
                length,
            } => {
                let v = self.eval(inner)?;
                let s = *start;
                let take = length.unwrap_or(v.len().saturating_sub(s));
                Ok(v.into_iter().skip(s).take(take).collect())
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let mut v = self.eval(inner)?;
                v.sort_by(|a, b| compare_by_keys(a, b, keys));
                Ok(v)
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                let v = self.eval(inner)?;
                let mut out = Vec::with_capacity(v.len());
                for mut b in v {
                    if let Some(t) = eval_expr_to_term(expr, &b)? {
                        b.set(var.name().to_owned(), t);
                    }
                    out.push(b);
                }
                Ok(out)
            }
            PhysicalPlan::Values { vars, rows } => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut b = Bindings::new();
                    for (var, cell) in vars.iter().zip(row.iter()) {
                        if let Some(term) = cell {
                            b.set(var.name().to_owned(), term.clone());
                        }
                    }
                    out.push(b);
                }
                Ok(out)
            }
            PhysicalPlan::Group {
                inner,
                keys,
                aggregates,
            } => {
                let v = self.eval(inner)?;
                eval_group(v, keys, aggregates)
            }
            PhysicalPlan::PathClosure {
                subject,
                object,
                edge,
                reflexive,
            } => {
                let edge_rows = self.eval(edge)?;
                eval_path_closure(subject, object, &edge_rows, *reflexive)
            }
        }
    }
}

/// Hash left-join (`OPTIONAL`): pair each left row with the compatible
/// right rows, preserving every left row that finds no match.
///
/// Replaces the original nested loop (O(|l|·|r|), which made trainmarks
/// q4 quadratic and time out at scale — #116). Two rows are compatible
/// iff they agree on every *shared* variable ([`Bindings::extend_compat`]);
/// any variable a left and right row could share is necessarily in the
/// **join-variable set** (the intersection of the two relations' bound
/// variables). So we index the right relation by the lexical values of
/// the join variables and probe only the matching bucket per left row,
/// turning the common (homogeneous, fully-bound) case into O(|l|+|r|).
///
/// Correctness is independent of the index: `extend_compat` still does
/// the final compatibility check and merge on every candidate pair, so
/// the index only ever *prunes* candidates that cannot match.
///
/// Rows that leave some join variable unbound can still be compatible on
/// the remaining shared variables, so they are handled conservatively: a
/// right row missing a join var goes to `unkeyed` and is a candidate for
/// *every* left row, and a left row missing a join var probes the whole
/// right relation. When there are no shared variables the single (empty)
/// key collapses to the cartesian product, matching `OPTIONAL` there.
fn hash_left_join(
    l: Vec<Bindings>,
    r: Vec<Bindings>,
    expr: Option<&Expr>,
) -> Result<Vec<Bindings>> {
    let jvars = join_vars(&l, &r);

    // Index the right relation by join-variable key; rows that don't bind
    // every join var can't be keyed and fall back to `unkeyed`.
    let mut index: HashMap<Vec<String>, Vec<&Bindings>> = HashMap::new();
    let mut unkeyed: Vec<&Bindings> = Vec::new();
    for b in &r {
        match join_key(b, &jvars) {
            Some(k) => index.entry(k).or_default().push(b),
            None => unkeyed.push(b),
        }
    }

    let mut out = Vec::new();
    for a in &l {
        let mut matched = false;
        match join_key(a, &jvars) {
            Some(k) => {
                if let Some(bucket) = index.get(&k) {
                    matched |= probe_into(a, bucket, expr, &mut out)?;
                }
                if !unkeyed.is_empty() {
                    matched |= probe_into(a, &unkeyed, expr, &mut out)?;
                }
            }
            // Left row missing a join var: it may still be compatible with
            // any right row on the remaining shared vars, so probe all.
            None => {
                let all: Vec<&Bindings> = r.iter().collect();
                matched |= probe_into(a, &all, expr, &mut out)?;
            }
        }
        if !matched {
            out.push(a.clone());
        }
    }
    Ok(out)
}

/// Probe one left row against a set of candidate right rows, pushing each
/// kept merged solution into `out`. Returns whether any candidate matched
/// (so the caller can decide to emit the unmatched left row). `expr` is
/// the `OPTIONAL`'s inner `FILTER`, evaluated over the merged row.
fn probe_into(
    a: &Bindings,
    candidates: &[&Bindings],
    expr: Option<&Expr>,
    out: &mut Vec<Bindings>,
) -> Result<bool> {
    let mut matched = false;
    for b in candidates {
        if let Some(m) = a.extend_compat(b) {
            let keep = match expr {
                Some(e) => eval_expr(e, &m)?,
                None => true,
            };
            if keep {
                matched = true;
                out.push(m);
            }
        }
    }
    Ok(matched)
}

/// The join-variable set: variables bound somewhere in *both* relations.
/// Sorted (via `BTreeSet`) so the key column order is deterministic.
fn join_vars(left: &[Bindings], right: &[Bindings]) -> Vec<String> {
    use std::collections::BTreeSet;
    let lvars: BTreeSet<&str> = left.iter().flat_map(|b| b.keys()).collect();
    let rvars: BTreeSet<&str> = right.iter().flat_map(|b| b.keys()).collect();
    lvars.intersection(&rvars).map(|s| s.to_string()).collect()
}

/// The probe/build key for a row: the lexical value of each join variable,
/// in `jvars` order. `None` if the row leaves any join variable unbound
/// (such rows can't be hash-keyed and take the conservative path).
fn join_key(b: &Bindings, jvars: &[String]) -> Option<Vec<String>> {
    let mut key = Vec::with_capacity(jvars.len());
    for v in jvars {
        key.push(lex(b.get(v)?));
    }
    Some(key)
}

/// Evaluate a recursive Kleene path `p+`/`p*`.
///
/// `edge_rows` are the one-step relation `p` denotes, each row binding
/// the hidden endpoint variables [`PATH_SRC_VAR`]/[`PATH_DST_VAR`].
/// We take the transitive closure of that relation by BFS to a fixpoint
/// (a `seen` set per source guarantees termination on cyclic data), and
/// — for `*` — add the reflexive pairs over every node the relation
/// touches. The resulting `(src, dst)` pairs are matched against the
/// query endpoints `subject`/`object`, each of which may be ground
/// (filter) or a variable (bind).
///
/// Stage-1 reflexive note: `p*`'s zero-length match is seeded only over
/// nodes that appear in the path relation (plus a ground endpoint, if
/// pinned), not over every node in the active graph. This matches the
/// documented approximation in [`crate::algebra::translate`]'s
/// `zero_length_path`; full graph-node enumeration for `*` is deferred.
fn eval_path_closure(
    subject: &Term,
    object: &Term,
    edge_rows: &[Bindings],
    reflexive: bool,
) -> Result<Vec<Bindings>> {
    use crate::algebra::{PATH_DST_VAR, PATH_SRC_VAR};
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    // The hidden endpoint variables are stored in `Bindings` under their
    // full names (the `?pp_*` sigil is part of the stored variable name,
    // since these are user-unspellable synthetic vars).
    let src_key = PATH_SRC_VAR;
    let dst_key = PATH_DST_VAR;

    // Adjacency over the lexical forms of the endpoint terms. We key on
    // the term's serialised form (`lex`) to dedupe, and keep a
    // representative `Term` for each node so we can rebuild bindings.
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut node_term: BTreeMap<String, Term> = BTreeMap::new();
    for row in edge_rows {
        let (Some(s), Some(o)) = (row.get(src_key), row.get(dst_key)) else {
            continue;
        };
        let (sk, ok) = (lex(s), lex(o));
        node_term.entry(sk.clone()).or_insert_with(|| s.clone());
        node_term.entry(ok.clone()).or_insert_with(|| o.clone());
        adj.entry(sk).or_default().insert(ok);
    }

    // Transitive closure: for each source, BFS over `adj`. Pairs are
    // keyed by lexical form; `closure` holds `(src_key, dst_key)`.
    let mut closure: BTreeSet<(String, String)> = BTreeSet::new();
    let sources: Vec<String> = adj.keys().cloned().collect();
    for start in sources {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        if let Some(nbrs) = adj.get(&start) {
            for n in nbrs {
                if seen.insert(n.clone()) {
                    queue.push_back(n.clone());
                }
            }
        }
        while let Some(cur) = queue.pop_front() {
            closure.insert((start.clone(), cur.clone()));
            if let Some(nbrs) = adj.get(&cur) {
                for n in nbrs {
                    if seen.insert(n.clone()) {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
    }

    // `*` adds the reflexive pairs over every node the relation touches.
    if reflexive {
        for k in node_term.keys() {
            closure.insert((k.clone(), k.clone()));
        }
        // A ground endpoint pinned to a node absent from the relation
        // still self-matches under the zero-length branch.
        for ep in [subject, object] {
            if !matches!(ep, Term::Var(_)) {
                let k = lex(ep);
                node_term.entry(k.clone()).or_insert_with(|| ep.clone());
                closure.insert((k.clone(), k));
            }
        }
    }

    // Bind/filter each closure pair against the query endpoints.
    let mut out = Vec::new();
    for (sk, ok) in &closure {
        let s_term = node_term.get(sk).cloned().unwrap();
        let o_term = node_term.get(ok).cloned().unwrap();
        let mut b = Bindings::new();
        if !bind_endpoint(subject, &s_term, &mut b) {
            continue;
        }
        if !bind_endpoint(object, &o_term, &mut b) {
            continue;
        }
        out.push(b);
    }
    Ok(out)
}

/// Match a closure endpoint against a query endpoint term, recording any
/// variable binding into `b`. Returns `false` if a ground query endpoint
/// does not equal the closure node (the pair is filtered out).
///
/// A repeated variable across both endpoints (e.g. `?x p+ ?x`) is handled
/// by `Bindings::set` overwriting with the same value only when the two
/// nodes agree — we guard that explicitly so an inconsistent pair is
/// dropped rather than silently keeping the second binding.
fn bind_endpoint(endpoint: &Term, node: &Term, b: &mut Bindings) -> bool {
    match endpoint {
        Term::Var(v) => {
            if let Some(existing) = b.get(v.name()) {
                return existing == node;
            }
            b.set(v.name().to_owned(), node.clone());
            true
        }
        ground => lex(ground) == lex(node),
    }
}

/// Evaluate `GROUP BY` + aggregates over a materialised input.
///
/// Rows are partitioned by the lexical form of the key-variable
/// bindings (an unbound key contributes a `None` slot, so rows that are
/// both unbound in a key fall in the same group). Implicit grouping
/// (`keys` empty) yields exactly one group — even over zero input rows,
/// per SPARQL 1.1 §11.2: `SELECT (COUNT(*) AS ?c) WHERE { … }` returns
/// a single row with `?c = 0` when nothing matches.
fn eval_group(
    rows: Vec<Bindings>,
    keys: &[Var],
    aggregates: &[Aggregate],
) -> Result<Vec<Bindings>> {
    // Group key -> (representative key bindings, member rows).
    // BTreeMap keeps output order deterministic.
    let mut groups: BTreeMap<Vec<Option<String>>, (Bindings, Vec<Bindings>)> = BTreeMap::new();

    for row in rows {
        let group_key: Vec<Option<String>> =
            keys.iter().map(|k| row.get(k.name()).map(lex)).collect();
        let entry = groups.entry(group_key).or_insert_with(|| {
            let mut key_bindings = Bindings::new();
            for k in keys {
                if let Some(t) = row.get(k.name()) {
                    key_bindings.set(k.name().to_owned(), t.clone());
                }
            }
            (key_bindings, Vec::new())
        });
        entry.1.push(row);
    }

    // Implicit grouping with no input rows still yields one (empty) group.
    if keys.is_empty() && groups.is_empty() {
        groups.insert(Vec::new(), (Bindings::new(), Vec::new()));
    }

    let mut out = Vec::with_capacity(groups.len());
    for (_, (mut binding, members)) in groups {
        for agg in aggregates {
            if let Some(t) = eval_aggregate(agg, &members)? {
                binding.set(agg.out.name().to_owned(), t);
            }
        }
        out.push(binding);
    }
    Ok(out)
}

/// Render an `xsd:integer` typed literal in N-Triples lexical form.
fn integer_literal(n: i64) -> Term {
    Term::Literal(format!(
        "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    ))
}

/// Render an `xsd:decimal` typed literal.
fn decimal_literal(x: f64) -> Term {
    Term::Literal(format!(
        "\"{x}\"^^<http://www.w3.org/2001/XMLSchema#decimal>"
    ))
}

/// Extract the lexical value of a literal term for numeric/string
/// comparison and aggregation. For a `"v"^^<dt>` or `"v"@lang` literal,
/// returns the inner `v`; for a plain literal (no quotes), returns it
/// as-is.
///
/// Stage-1 note: the `MemStore` erases term kinds on scan, so a bound
/// literal object arrives as `Term::Iri("\"10\"^^<…>")` — the literal's
/// full N-Triples form wrapped in the wrong variant. We therefore run
/// `literal_lexical` over the `Iri`/`BlankNode` lexical forms too; a
/// genuine IRI does not start with `"` so it is returned unchanged. Once
/// the term-kind preservation (rung 4 / SPEC-02) lands this collapses to
/// just the `Literal` arm.
fn literal_value(t: &Term) -> String {
    match t {
        Term::Literal(raw) => literal_lexical(raw),
        Term::Iri(s) | Term::BlankNode(s) => literal_lexical(s),
        Term::Var(v) => v.name().to_owned(),
        Term::Triple(_) => String::new(),
    }
}

/// Decode N-Triples string escapes (`\\`, `\"`, `\n`, `\t`, `\r`,
/// `\uXXXX`, `\UXXXXXXXX`) in a literal's lexical form. Unknown
/// escapes pass through verbatim (best-effort, mirroring the lenient
/// Stage-1 parsing elsewhere).
pub(crate) fn unescape_ntriples(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000C}'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some(u @ ('u' | 'U')) => {
                let len = if u == 'u' { 4 } else { 8 };
                let hex: String = chars.by_ref().take(len).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => out.push(decoded),
                    None => {
                        out.push('\\');
                        out.push(u);
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse the lexical part out of an N-Triples literal string.
fn literal_lexical(raw: &str) -> String {
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return raw.to_owned();
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return unescape_ntriples(&raw[1..i]);
        }
        i += 1;
    }
    raw.to_owned()
}

/// Best-effort numeric coercion of a term for SUM/AVG/MIN/MAX.
fn numeric_value(t: &Term) -> Option<f64> {
    literal_value(t).trim().parse::<f64>().ok()
}

/// The numeric XSD datatypes for datatype-aware EBV and `ISNUMERIC`.
fn is_numeric_datatype(dt: &str) -> bool {
    let Some(local) = dt.strip_prefix("http://www.w3.org/2001/XMLSchema#") else {
        return false;
    };
    matches!(
        local,
        "integer"
            | "decimal"
            | "double"
            | "float"
            | "long"
            | "int"
            | "short"
            | "byte"
            | "nonNegativeInteger"
            | "nonPositiveInteger"
            | "negativeInteger"
            | "positiveInteger"
            | "unsignedLong"
            | "unsignedInt"
            | "unsignedShort"
            | "unsignedByte"
    )
}

/// SPARQL effective boolean value (§17.2.2), datatype-aware:
/// `xsd:boolean` → its value, numeric datatypes → value ≠ 0, plain /
/// `xsd:string` / lang-tagged → non-empty lexical form (so the *string*
/// `"false"` is true). EBV of a non-literal (IRI / blank node) or of a
/// non-boolean/numeric/string datatype is a type error — under the
/// crate-wide error→false convention it yields `false`.
fn ebv(t: &Term) -> bool {
    if term_kind(t) != TermKind::Literal {
        return false;
    }
    let raw = lex(t);
    if !raw.starts_with('"') {
        // Internal unquoted boolean results (`bool_lit`, the
        // comparison-expression terms): not an N-Triples form, so
        // keep the legacy lexical rules.
        return match raw.as_str() {
            "true" => true,
            "false" => false,
            other => match other.trim().parse::<f64>() {
                Ok(n) => n != 0.0,
                Err(_) => !other.is_empty(),
            },
        };
    }
    let (value, _lang, dt) = literal_parts(&raw);
    match dt.as_deref() {
        Some("http://www.w3.org/2001/XMLSchema#boolean") => value == "true" || value == "1",
        Some(dt) if is_numeric_datatype(dt) => value
            .trim()
            .parse::<f64>()
            .map(|n| n != 0.0 && !n.is_nan())
            .unwrap_or(false),
        Some("http://www.w3.org/2001/XMLSchema#string") | None => !value.is_empty(),
        Some(_) => false, // other datatypes: type error
    }
}

/// Wrap a lexical value as a plain (unquoted-form) literal term,
/// re-applying N-Triples string escapes so the stored form round-trips
/// through `literal_lexical`.
fn plain_literal(s: &str) -> Term {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{0008}', "\\b")
        .replace('\u{000C}', "\\f");
    Term::Literal(format!("\"{escaped}\""))
}

/// Binary arithmetic over the Stage-1 f64 model. `None` (expression
/// error) when either side is non-numeric or on division by zero.
/// Overflow/NaN edge cases (e.g. inf - inf) are accepted Stage-1 f64-model
/// behavior and can render as "NaN"/"inf" literals.
fn arith(op: fn(f64, f64) -> f64, a: Option<f64>, b: Option<f64>) -> Option<Term> {
    Some(numeric_term(op(a?, b?)))
}

/// Split an N-Triples literal raw form into (lexical, lang, datatype).
/// Non-literal raw forms (no leading quote) yield (raw, None, None).
pub(crate) fn literal_parts(raw: &str) -> (String, Option<String>, Option<String>) {
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
/// (expression error). Called per evaluated row — a compiled-pattern
/// cache is a future optimisation once FILTER throughput matters
/// (Stage-1 result sets are small).
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

/// Compute one aggregate over a group's member rows.
fn eval_aggregate(agg: &Aggregate, members: &[Bindings]) -> Result<Option<Term>> {
    // Collect the aggregate's input multiset (the values of the inner
    // expression over the members), applying DISTINCT if requested.
    // For COUNT(*) the "input" is the rows themselves.
    let collect_values = |expr: &Expr| -> Result<Vec<Term>> {
        let mut vals = Vec::new();
        for m in members {
            if let Some(t) = eval_expr_to_term(expr, m)? {
                vals.push(t);
            }
        }
        if agg.distinct {
            dedup_terms(&mut vals);
        }
        Ok(vals)
    };

    Ok(match &agg.func {
        AggFunc::CountStar => {
            let n = if agg.distinct {
                // COUNT(DISTINCT *) — distinct whole solution rows.
                let mut seen: Vec<&Bindings> = Vec::new();
                for m in members {
                    if !seen.contains(&m) {
                        seen.push(m);
                    }
                }
                seen.len()
            } else {
                members.len()
            };
            Some(integer_literal(n as i64))
        }
        AggFunc::Count(e) => {
            let vals = collect_values(e)?;
            Some(integer_literal(vals.len() as i64))
        }
        AggFunc::Sum(e) => {
            let vals = collect_values(e)?;
            let sum: f64 = vals.iter().filter_map(numeric_value).sum();
            Some(numeric_term(sum))
        }
        AggFunc::Avg(e) => {
            let vals = collect_values(e)?;
            let nums: Vec<f64> = vals.iter().filter_map(numeric_value).collect();
            if nums.is_empty() {
                Some(integer_literal(0))
            } else {
                Some(decimal_literal(
                    nums.iter().sum::<f64>() / nums.len() as f64,
                ))
            }
        }
        AggFunc::Min(e) => {
            let vals = collect_values(e)?;
            aggregate_extreme(&vals, true)
        }
        AggFunc::Max(e) => {
            let vals = collect_values(e)?;
            aggregate_extreme(&vals, false)
        }
        AggFunc::Sample(e) => {
            let vals = collect_values(e)?;
            vals.into_iter().next()
        }
        AggFunc::GroupConcat { expr, separator } => {
            let vals = collect_values(expr)?;
            let joined = vals
                .iter()
                .map(literal_value)
                .collect::<Vec<_>>()
                .join(separator);
            Some(Term::Literal(format!(
                "\"{}\"",
                joined.replace('"', "\\\"")
            )))
        }
    })
}

/// Pick MIN (`min == true`) or MAX of an input multiset. Numeric when
/// every value parses as a number, otherwise lexical ordering.
fn aggregate_extreme(vals: &[Term], min: bool) -> Option<Term> {
    if vals.is_empty() {
        return None;
    }
    let all_numeric = vals.iter().all(|t| numeric_value(t).is_some());
    if all_numeric {
        let mut best_idx = 0;
        let mut best = numeric_value(&vals[0]).unwrap();
        for (i, t) in vals.iter().enumerate().skip(1) {
            let n = numeric_value(t).unwrap();
            if (min && n < best) || (!min && n > best) {
                best = n;
                best_idx = i;
            }
        }
        Some(vals[best_idx].clone())
    } else {
        let mut best = &vals[0];
        for t in &vals[1..] {
            let ord = lex(t).cmp(&lex(best));
            if (min && ord == std::cmp::Ordering::Less)
                || (!min && ord == std::cmp::Ordering::Greater)
            {
                best = t;
            }
        }
        Some(best.clone())
    }
}

/// Render a numeric aggregate result as an integer literal when it has
/// no fractional part, otherwise a decimal literal.
fn numeric_term(x: f64) -> Term {
    if x.fract() == 0.0 && x.abs() < 9.007e15 {
        integer_literal(x as i64)
    } else {
        decimal_literal(x)
    }
}

/// Deduplicate a term multiset by value, preserving first-seen order.
fn dedup_terms(vals: &mut Vec<Term>) {
    let mut seen: Vec<Term> = Vec::new();
    vals.retain(|t| {
        if seen.contains(t) {
            false
        } else {
            seen.push(t.clone());
            true
        }
    });
}

fn project(b: &Bindings, vars: &[Var]) -> Bindings {
    if vars.is_empty() {
        // SELECT * with no projected vars (e.g. ASK): preserve.
        return b.clone();
    }
    let mut out = Bindings::new();
    for v in vars {
        if let Some(t) = b.get(v.name()) {
            out.set(v.name().to_owned(), t.clone());
        }
    }
    out
}

fn compare_by_keys(a: &Bindings, b: &Bindings, keys: &[(Expr, OrderDir)]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (e, dir) in keys {
        let ta = eval_expr_to_term(e, a).ok().flatten();
        let tb = eval_expr_to_term(e, b).ok().flatten();
        let ord = match (ta, tb) {
            (Some(x), Some(y)) => compare_terms(&x, &y),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return match dir {
                OrderDir::Asc => ord,
                OrderDir::Desc => ord.reverse(),
            };
        }
    }
    std::cmp::Ordering::Equal
}

fn lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
        // RDF 1.2 triple terms have no canonical lexical form in the
        // Stage-1 String-based representation. Emitting the empty
        // string here is consistent with how unbound `Var` patterns
        // surface in lexicographic comparisons; SPEC-07 RDF 1.2
        // follow-up will route this through the dictionary instead.
        Term::Triple(_) => String::new(),
    }
}

fn eval_expr(e: &Expr, b: &Bindings) -> Result<bool> {
    use std::cmp::Ordering;
    let cmp = |a: &Expr, c: &Expr| -> Result<Option<Ordering>> {
        Ok(match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => Some(compare_terms(&x, &y)),
            _ => None,
        })
    };
    Ok(match e {
        Expr::Eq(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::Ne(a, c) => eval_expr_to_term(a, b)? != eval_expr_to_term(c, b)?,
        Expr::Lt(a, c) => cmp(a, c)? == Some(Ordering::Less),
        Expr::Gt(a, c) => cmp(a, c)? == Some(Ordering::Greater),
        Expr::Le(a, c) => matches!(cmp(a, c)?, Some(Ordering::Less | Ordering::Equal)),
        Expr::Ge(a, c) => matches!(cmp(a, c)?, Some(Ordering::Greater | Ordering::Equal)),
        Expr::And(a, c) => eval_expr(a, b)? && eval_expr(c, b)?,
        Expr::Or(a, c) => eval_expr(a, b)? || eval_expr(c, b)?,
        Expr::Not(a) => !eval_expr(a, b)?,
        Expr::Bound(v) => b.get(v.name()).is_some(),
        Expr::In(a, list) => {
            let lhs = eval_expr_to_term(a, b)?;
            match lhs {
                None => false,
                Some(x) => {
                    let mut found = false;
                    for item in list {
                        if let Some(y) = eval_expr_to_term(item, b)? {
                            // Value equality (not variant equality): the
                            // Stage-1 store may bind the LHS as a
                            // different term kind than the constant RHS.
                            if x == y || compare_terms(&x, &y) == std::cmp::Ordering::Equal {
                                found = true;
                                break;
                            }
                        }
                    }
                    found
                }
            }
        }
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
        Expr::Term(t) => match t {
            // Bare term in boolean context: SPARQL effective boolean
            // value of the bound value (unbound var is an error →
            // false) or of the constant itself.
            Term::Var(v) => b.get(v.name()).map(ebv).unwrap_or(false),
            other => ebv(other),
        },
    })
}

/// Order two terms for SPARQL relational operators. Numeric when both
/// coerce to numbers, then xsd:dateTime when both look like ISO-8601
/// instants, otherwise lexical comparison of the literal value. This is
/// a Stage-1 best effort — it covers the SPB datetime-range filters and
/// ordinary numeric/string comparisons without a full XSD type lattice.
fn compare_terms(x: &Term, y: &Term) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let (Some(a), Some(b)) = (numeric_value(x), numeric_value(y)) {
        return a.partial_cmp(&b).unwrap_or(Ordering::Equal);
    }
    let (lx, ly) = (literal_value(x), literal_value(y));
    if let (Some(a), Some(b)) = (datetime_key(&lx), datetime_key(&ly)) {
        return a.cmp(&b);
    }
    lx.cmp(&ly)
}

/// Normalise an xsd:dateTime lexical form into a sortable key. Returns
/// `None` if the string does not look like an ISO-8601 instant. We do
/// not parse offsets fully; the lexical form sorts correctly for the
/// common `YYYY-MM-DDThh:mm:ss(.fff)?(Z)?` shape used by SPB, so we just
/// validate the prefix and key on the original string.
fn datetime_key(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    // Minimum `YYYY-MM-DDThh:mm:ss` is 19 chars.
    if bytes.len() < 19 {
        return None;
    }
    let is_shape = bytes[4] == b'-'
        && bytes[7] == b'-'
        && (bytes[10] == b'T' || bytes[10] == b' ')
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[..4].iter().all(|c| c.is_ascii_digit());
    if is_shape {
        Some(s.to_owned())
    } else {
        None
    }
}

fn eval_expr_to_term(e: &Expr, b: &Bindings) -> Result<Option<Term>> {
    // Evaluate an operand to its numeric value; an expression error
    // (non-numeric / unbound) surfaces as `Ok(None)`.
    let numof = |sub: &Expr| -> Result<Option<f64>> {
        Ok(eval_expr_to_term(sub, b)?.as_ref().and_then(numeric_value))
    };
    Ok(match e {
        Expr::Term(t) => match t {
            Term::Var(v) => b.get(v.name()).cloned(),
            other => Some(other.clone()),
        },
        // Boolean-typed expressions return a typed literal (lexical
        // form "true"/"false"); good enough for Stage 1 BIND tests.
        Expr::Eq(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
        | Expr::Gt(_, _)
        | Expr::Le(_, _)
        | Expr::Ge(_, _)
        | Expr::In(_, _)
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::Bound(_) => Some(Term::Literal(
            if eval_expr(e, b)? { "true" } else { "false" }.into(),
        )),
        Expr::Add(x, y) => arith(|a, b| a + b, numof(x)?, numof(y)?),
        Expr::Sub(x, y) => arith(|a, b| a - b, numof(x)?, numof(y)?),
        Expr::Mul(x, y) => arith(|a, b| a * b, numof(x)?, numof(y)?),
        Expr::Div(x, y) => match numof(y)? {
            Some(d) if d != 0.0 => arith(|a, b| a / b, numof(x)?, Some(d)),
            _ => None, // division by zero / non-numeric divisor
        },
        Expr::Neg(x) => numof(x)?.map(|n| numeric_term(-n)),
        // Stage-1 note: an erroring condition evaluates as false (the
        // crate-wide error→false EBV convention) and takes the else
        // branch, rather than propagating the error as SPARQL §17.4.1.2
        // specifies.
        Expr::If(c, t, f) => {
            if eval_expr(c, b)? {
                eval_expr_to_term(t, b)?
            } else {
                eval_expr_to_term(f, b)?
            }
        }
        Expr::Coalesce(args) => {
            // `?` is safe here because runtime expression errors are represented
            // as Ok(None), never Err — so error-skipping per SPARQL §17.4.1.6
            // still holds.
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
    })
}

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
    let num = |i: usize| -> Result<Option<f64>> { Ok(term(i)?.as_ref().and_then(numeric_value)) };
    let bool_lit = |v: bool| Some(Term::Literal(if v { "true" } else { "false" }.into()));

    Ok(match f {
        Func::Str => term(0)?.map(|t| plain_literal(&literal_value(&t))),
        Func::Lang => term(0)?.and_then(|t| {
            // LANG on a non-literal is a type error (SPARQL §17.4.1.1),
            // mirroring the DATATYPE arm below.
            if term_kind(&t) != TermKind::Literal {
                return None;
            }
            let (_, lang, _) = literal_parts(&lex(&t));
            Some(plain_literal(&lang.unwrap_or_default()))
        }),
        // RFC 4647 *basic* filtering per SPARQL §17.4.3.7: "*" matches
        // any non-empty tag, otherwise exact or prefix-before-'-'
        // match. Extended ranges with embedded wildcards ("en-*") are
        // deliberately out of scope — basic filtering does not define
        // them.
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
        // fn:round rounds half toward positive infinity (ROUND(-2.5) =
        // -2), unlike Rust's round-half-away-from-zero.
        Func::Round => num(0)?.map(|n| numeric_term((n + 0.5).floor())),
        Func::IsIri => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Iri)),
        Func::IsBlank => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Blank)),
        Func::IsLiteral => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Literal)),
        // ISNUMERIC is true only for literals with a numeric XSD
        // datatype whose lexical form parses (§17.4.2.4) — a plain
        // string that merely looks numeric ("42") is false.
        Func::IsNumeric => term(0)?.and_then(|t| {
            if term_kind(&t) != TermKind::Literal {
                return bool_lit(false);
            }
            let (value, _, dt) = literal_parts(&lex(&t));
            let ok = dt.as_deref().is_some_and(is_numeric_datatype)
                && value.trim().parse::<f64>().is_ok();
            bool_lit(ok)
        }),
        Func::Year | Func::Month | Func::Day | Func::Hours | Func::Minutes | Func::Seconds => {
            // The accessors are defined on xsd:dateTime — a plain
            // string that merely looks like a timestamp is a type
            // error, matching the ISNUMERIC datatype strictness.
            let t = match term(0)? {
                Some(t) => t,
                None => return Ok(None),
            };
            let (v, _, dt) = literal_parts(&lex(&t));
            if dt.as_deref() != Some("http://www.w3.org/2001/XMLSchema#dateTime") {
                return Ok(None);
            }
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
                    // SECONDS — keep any fractional part. Always
                    // xsd:decimal per SPARQL §17.4.5.6 (numeric_term
                    // would promote whole seconds to xsd:integer).
                    let tail: String = v[17..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    tail.parse::<f64>().ok().map(decimal_literal)
                }
            }
        }
    })
}

// Type-witness so we don't drop SparqlError from this module.
#[allow(dead_code)]
fn _witness() -> Result<()> {
    Err(SparqlError::Executor("unreachable".into()))
}

/// Render a CONSTRUCT template against a stream of solution mappings.
///
/// Returns concrete `(s, p, o)` lexical-form triples. Triples whose
/// template references an unbound variable in the row are skipped
/// (W3C: "ground triple results only").
pub fn construct_triples(
    query: &spargebra::Query,
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use spargebra::term::{NamedNodePattern, TermPattern};
    let template = match query {
        spargebra::Query::Construct { template, .. } => template,
        _ => {
            return Err(SparqlError::Executor(
                "construct_triples called on non-CONSTRUCT query".into(),
            ))
        }
    };

    fn resolve_term(t: &TermPattern, row: &Bindings) -> Option<String> {
        match t {
            TermPattern::NamedNode(n) => Some(n.as_str().to_owned()),
            TermPattern::BlankNode(b) => Some(b.as_str().to_owned()),
            TermPattern::Literal(l) => Some(l.to_string()),
            TermPattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => {
                    Some(s.clone())
                }
                _ => None,
            },
            // RDF 1.2 ground triple-term templates in CONSTRUCT are not
            // emitted by the Stage-1 lexical-form path (a `Term::Triple`
            // has no canonical `String` form here). Skip the slot so the
            // outer (s, p, o) tuple is dropped. See SPEC-07 / TASKS.md
            // for the dictionary-backed CONSTRUCT follow-up.
            TermPattern::Triple(_) => None,
        }
    }
    // See also update.rs::resolve_pred — same "predicate var binding must
    // be an IRI" invariant; keep the two in lockstep.
    fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<String> {
        match p {
            NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
            NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) => Some(s.clone()),
                _ => None,
            },
        }
    }

    let mut out = Vec::new();
    for row in rows {
        for tp in template {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&tp.subject, row),
                resolve_pred(&tp.predicate, row),
                resolve_term(&tp.object, row),
            ) {
                out.push((s, p, o));
            }
        }
    }
    Ok(out)
}

/// Build a DESCRIBE result graph from explicit-IRI seeds plus
/// already-projected solution rows.
///
/// `seeds` are resources named directly by IRI in the DESCRIBE clause
/// (SPARQL 1.1 §16.4); they are described unconditionally — even when the
/// WHERE clause yields zero rows. The `rows` arrive projected to the
/// DESCRIBE-target variables (the planner runs the same projection as a
/// SELECT), so every value bound to *any* variable in a row is also a
/// resource to describe. The final resource set is (seeds) ∪ (row
/// bindings), deduplicated. We emit a
/// **forward, one-level Concise Bounded Description**: for each distinct
/// resource, every stored triple with that resource as subject.
///
/// Output is deduplicated and returned in deterministic sorted order
/// (via `BTreeSet`). Literals bound to a projected variable are never
/// subjects of stored triples, so they naturally contribute nothing —
/// no special-casing needed.
///
/// Each describe-target resource is scanned with its **original term**
/// (kind preserved), so a type-preserving backend that binds a target
/// to a `Term::BlankNode` is scanned as a blank node, not coerced to an
/// IRI. The Stage-1 `MemStore` erases term kinds on scan (`unify_one`
/// binds every value as `Term::Iri(lexical)`), which masks the
/// distinction there but not for richer backends.
///
/// Deferred (out of scope, see SPEC-07 / TASKS.md): recursive
/// blank-node CBD closure and symmetric CBD (would require reliably
/// detecting blank-node objects to recurse into, which the term-kind
/// erasure in `MemStore` defeats). Typed-literal / Turtle serialisation
/// is likewise a separate increment (#57); this reuses the N-Triples
/// path.
pub fn describe_triples<E: Executor + ?Sized>(
    exec: &E,
    seeds: &[Term],
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use crate::algebra::{Term, TriplePattern, Var};
    use std::collections::{BTreeSet, HashSet};

    // Variable names used in the forward-scan pattern below. Defined once
    // so the pattern construction and the binding lookups can't drift.
    const PRED_VAR: &str = "p";
    const OBJ_VAR: &str = "o";

    // Distinct resource *terms* (kind preserved) bound across all rows /
    // all vars. Scanning with the original term keeps a `Term::BlankNode`
    // target from being silently coerced to a `Term::Iri`, which would
    // miss its triples on a kind-preserving backend.
    let mut resources: HashSet<Term> = HashSet::new();
    // Resources named directly by IRI in the DESCRIBE clause (SPARQL 1.1
    // §16.4). These are described unconditionally, independent of whether
    // the WHERE clause produced any solution rows.
    for term in seeds {
        match term {
            Term::Iri(_) | Term::Literal(_) | Term::BlankNode(_) => {
                resources.insert(term.clone());
            }
            Term::Var(_) | Term::Triple(_) => {}
        }
    }
    for row in rows {
        for (_name, term) in row.vars() {
            match term {
                Term::Iri(_) | Term::Literal(_) | Term::BlankNode(_) => {
                    resources.insert(term.clone());
                }
                // An unbound var or a triple-term can't be a describe
                // subject, so it carries no describable resource here.
                Term::Var(_) | Term::Triple(_) => {}
            }
        }
    }

    // Lexical form of a resource term, used as the subject of every
    // emitted triple. Only the three scannable kinds reach here.
    fn subject_lex(term: &Term) -> Option<&str> {
        match term {
            Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => Some(s),
            Term::Var(_) | Term::Triple(_) => None,
        }
    }

    let mut out: BTreeSet<(String, String, String)> = BTreeSet::new();
    for resource in &resources {
        let Some(subject) = subject_lex(resource) else {
            continue;
        };
        let pattern = TriplePattern {
            subject: resource.clone(),
            predicate: Term::Var(Var::new(PRED_VAR)),
            object: Term::Var(Var::new(OBJ_VAR)),
        };
        for b in exec.scan_bgp(std::slice::from_ref(&pattern))? {
            let p = match b.get(PRED_VAR) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => s.clone(),
                _ => continue,
            };
            let o = match b.get(OBJ_VAR) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => s.clone(),
                _ => continue,
            };
            out.insert((subject.to_owned(), p, o));
        }
    }
    Ok(out.into_iter().collect())
}
