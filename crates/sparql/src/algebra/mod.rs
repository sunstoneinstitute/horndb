//! Internal SPARQL algebra. A stable subset of `spargebra::algebra`.
//!
//! Why our own enum and not raw `spargebra::algebra::GraphPattern`?
//! Two reasons:
//!   * we want to constrain which operators the planner/executor are
//!     allowed to see (Stage 1 supports a smaller set than spargebra
//!     can produce);
//!   * upstream variants change between patch releases — keeping our
//!     algebra owned in-crate localises the breakage.

pub mod translate;

use std::sync::Arc;

/// A SPARQL variable. Stored as an interned name; equality is by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Var(Arc<str>);

impl Var {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// A SPARQL term as it appears inside a triple pattern.
///
/// We hold IRIs and string-form literals as owned strings in Stage 1.
/// SPEC-02 will replace these with dictionary IDs; the algebra is
/// allowed to carry either via the `Term` enum extending later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    Var(Var),
    Iri(String),
    BlankNode(String),
    /// A literal in N-Triples lexical form, e.g. `"hello"` or
    /// `"42"^^<http://www.w3.org/2001/XMLSchema#integer>`.
    Literal(String),
    /// An RDF 1.2 triple term (only emitted when `SparqlConfig::rdf12`
    /// is enabled — the translator rejects it otherwise). The inner
    /// `TriplePattern` may itself be variable-shaped, so this carries
    /// a full sub-pattern, not just ground triples.
    Triple(Box<TriplePattern>),
}

/// A SPARQL triple pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TriplePattern {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
}

/// A SPARQL expression — Stage 1 covers comparisons, boolean connectives,
/// arithmetic, IF, COALESCE and the common builtin functions. EXISTS,
/// non-deterministic builtins and custom functions are out of scope.
/// See [`Func`] for the full builtin list.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Term(Term),
    Eq(Box<Expr>, Box<Expr>),
    /// Term equality (`sameTerm`) — the strength-reduced form of `Eq` for
    /// operands the type lattice proves are the same non-literal kind
    /// (`plan::passes::normalize`). Evaluated as structural `Term`
    /// equality, identical to `Eq` today; the two diverge only once `Eq`
    /// gains value-equality (numeric promotion) semantics.
    SameTerm(Box<Expr>, Box<Expr>),
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

/// Algebra operators supported in Stage 1.
///
/// Notable omissions vs the full W3C algebra:
///   * Group/Aggregate (no GROUP BY in Stage 1)
///   * Minus
///   * Service
///
/// Property paths lower in [`translate`]: the non-recursive operators
/// `/` (Seq) and `^` (Inverse) collapse into expanded triple patterns;
/// `|` (Alt) and `?` (ZeroOrOne) lower to `Union`; `!`
/// (NegatedPropertySet) lowers to a wildcard-predicate BGP under a
/// `NOT IN` filter. The recursive Kleene operators `*`/`+` lower to the
/// [`Algebra::PathClosure`] node, which the runtime evaluates by a
/// fixpoint over the one-step edge relation.
#[derive(Debug, Clone, PartialEq)]
pub enum Algebra {
    Bgp {
        patterns: Vec<TriplePattern>,
    },
    Join {
        left: Box<Algebra>,
        right: Box<Algebra>,
    },
    LeftJoin {
        left: Box<Algebra>,
        right: Box<Algebra>,
        expr: Option<Expr>,
    },
    Filter {
        expr: Expr,
        inner: Box<Algebra>,
    },
    Union {
        left: Box<Algebra>,
        right: Box<Algebra>,
    },
    Project {
        vars: Vec<Var>,
        inner: Box<Algebra>,
    },
    Distinct {
        inner: Box<Algebra>,
    },
    Slice {
        inner: Box<Algebra>,
        start: usize,
        length: Option<usize>,
    },
    OrderBy {
        inner: Box<Algebra>,
        keys: Vec<(Expr, OrderDir)>,
    },
    /// `BIND (?e AS ?v)` and `VALUES`-style row sets reduce to Extend.
    Extend {
        inner: Box<Algebra>,
        var: Var,
        expr: Expr,
    },
    Values {
        vars: Vec<Var>,
        rows: Vec<Vec<Option<Term>>>,
    },
    /// `GROUP BY` + aggregates. `keys` are the grouping variables (empty
    /// for implicit grouping, e.g. `SELECT (COUNT(*) AS ?c) WHERE {…}`,
    /// which yields a single group). Each output row carries the key
    /// bindings plus one binding per `aggregate`.
    Group {
        inner: Box<Algebra>,
        keys: Vec<Var>,
        aggregates: Vec<Aggregate>,
    },
    /// Recursive Kleene property path `p+` / `p*` between `subject` and
    /// `object`.
    ///
    /// `edge` is the one-step relation the inner path `p` denotes,
    /// already lowered to an [`Algebra`] over the two hidden endpoint
    /// variables [`PATH_SRC_VAR`] (source) and [`PATH_DST_VAR`]
    /// (destination). The runtime materialises `edge`, takes its
    /// transitive closure (BFS to a fixpoint, so cycles terminate), and
    /// — when `reflexive` is set (`*`) — adds the zero-length pairs over
    /// the nodes the path touches. The closure rows are then matched
    /// against `subject`/`object`, each of which may be ground or a
    /// variable.
    PathClosure {
        subject: Term,
        object: Term,
        edge: Box<Algebra>,
        /// `true` for `*` (reflexive-transitive), `false` for `+`
        /// (transitive only).
        reflexive: bool,
    },
}

/// Hidden source-endpoint variable threaded through a
/// [`Algebra::PathClosure`]'s `edge` sub-plan. User-unspellable (the
/// `?` sigil cannot appear in a parsed variable name).
pub const PATH_SRC_VAR: &str = "?pp_src";
/// Hidden destination-endpoint variable for a [`Algebra::PathClosure`]
/// `edge` sub-plan. User-unspellable, paired with [`PATH_SRC_VAR`].
pub const PATH_DST_VAR: &str = "?pp_dst";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDir {
    Asc,
    Desc,
}

/// A single aggregate to compute over a group, with the output variable
/// it is bound to. `distinct` applies the SPARQL DISTINCT modifier to
/// the aggregate's input multiset before folding.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// The variable the aggregate's value is bound to in the output row.
    pub out: Var,
    pub func: AggFunc,
    /// `true` for `COUNT(DISTINCT ?x)`, `SUM(DISTINCT ?x)`, etc.
    pub distinct: bool,
}

/// The aggregate functions Stage 1 evaluates.
#[derive(Debug, Clone, PartialEq)]
pub enum AggFunc {
    /// `COUNT(*)` — count solutions, no inner expression.
    CountStar,
    /// `COUNT(?x)` — count rows where the expression is bound.
    Count(Box<Expr>),
    Sum(Box<Expr>),
    Min(Box<Expr>),
    Max(Box<Expr>),
    Avg(Box<Expr>),
    Sample(Box<Expr>),
    GroupConcat {
        expr: Box<Expr>,
        separator: String,
    },
}
