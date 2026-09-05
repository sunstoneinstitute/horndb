//! SPARQL numeric values: one type for arithmetic, the rounding builtins and
//! the numeric aggregates.
//!
//! SPARQL 1.1 §17.4.1 defines its operators by the XPath *operator mapping*:
//! an operand's `xsd` datatype decides which XPath function runs, and mixed
//! operands are promoted to the first type that holds both. The lattice is
//!
//! ```text
//! integer  <  decimal  <  float  <  double
//! ```
//!
//! so `1 + 2` stays `xsd:integer`, `1.0 + 2` is `xsd:decimal`, and anything
//! mixed with an `xsd:double` is `xsd:double`. Division is the one exception:
//! `integer / integer` yields `xsd:decimal` (`op:numeric-divide`).
//!
//! Each variant keeps the exact value of its type — `xsd:decimal` is fixed
//! point, never `f64` — so summing decimals gives `11.1`, not
//! `11.100000000000001`. Rendering goes through the XSD canonical lexical
//! form: `xsd:double` is always `<mantissa>E<exponent>` with a fractional
//! digit (`2.0E-1`, not `0.2` or `2E-1`).

use std::str::FromStr;

use oxsdatatypes::{Decimal, Double, Float, Integer};

use crate::algebra::Term;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// A SPARQL numeric value, tagged with the `xsd` type it is operated on as.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Numeric {
    Int(Integer),
    Dec(Decimal),
    Flt(Float),
    Dbl(Double),
}

impl Numeric {
    /// The additive identity, and the value `SUM` of an empty multiset
    /// returns (SPARQL 1.1 §18.5.1.5).
    pub(crate) fn zero() -> Self {
        Numeric::from_i64(0)
    }

    /// Read a literal's lexical form under its datatype IRI. `None` when the
    /// datatype is not one the operator mapping accepts (a plain literal, an
    /// `xsd:string`, an IRI) or when the lexical form does not parse — both
    /// are SPARQL type errors.
    ///
    /// The derived integer types (`xsd:long`, `xsd:int`, `xsd:byte`, the
    /// `…Integer` restrictions, the `unsigned…` family) map to `xsd:integer`
    /// operands, per the operator mapping's substitution rule.
    pub(crate) fn parse(value: &str, datatype: &str) -> Option<Self> {
        let local = datatype.strip_prefix(XSD)?;
        let v = value.trim();
        match local {
            "integer" | "long" | "int" | "short" | "byte" | "nonNegativeInteger"
            | "nonPositiveInteger" | "negativeInteger" | "positiveInteger" | "unsignedLong"
            | "unsignedInt" | "unsignedShort" | "unsignedByte" => {
                Integer::from_str(v).ok().map(Numeric::Int)
            }
            "decimal" => Decimal::from_str(v).ok().map(Numeric::Dec),
            "float" => Float::from_str(v).ok().map(Numeric::Flt),
            "double" => Double::from_str(v).ok().map(Numeric::Dbl),
            _ => None,
        }
    }

    /// An `xsd:integer` value — the inline-int fast path's entry point.
    pub(crate) fn from_i64(v: i64) -> Self {
        Numeric::Int(Integer::from(v))
    }

    /// Where this value sits in the promotion lattice.
    fn rank(self) -> u8 {
        match self {
            Numeric::Int(_) => 0,
            Numeric::Dec(_) => 1,
            Numeric::Flt(_) => 2,
            Numeric::Dbl(_) => 3,
        }
    }

    /// Widen to `rank`, which must not be below this value's own rank.
    fn widen(self, rank: u8) -> Self {
        match (self, rank) {
            (Numeric::Int(i), 1) => Numeric::Dec(i.into()),
            (Numeric::Int(i), 2) => Numeric::Flt(i.into()),
            (Numeric::Int(i), 3) => Numeric::Dbl(i.into()),
            (Numeric::Dec(d), 2) => Numeric::Flt(d.into()),
            (Numeric::Dec(d), 3) => Numeric::Dbl(d.into()),
            (Numeric::Flt(f), 3) => Numeric::Dbl(f.into()),
            (same, _) => same,
        }
    }

    /// Both operands at their common type.
    fn promote(self, other: Self) -> (Self, Self) {
        let rank = self.rank().max(other.rank());
        (self.widen(rank), other.widen(rank))
    }

    /// The XSD datatype IRI of the result literal.
    fn datatype(self) -> &'static str {
        match self {
            Numeric::Int(_) => "http://www.w3.org/2001/XMLSchema#integer",
            Numeric::Dec(_) => "http://www.w3.org/2001/XMLSchema#decimal",
            Numeric::Flt(_) => "http://www.w3.org/2001/XMLSchema#float",
            Numeric::Dbl(_) => "http://www.w3.org/2001/XMLSchema#double",
        }
    }

    /// The XSD canonical lexical form.
    pub(crate) fn lexical(self) -> String {
        match self {
            Numeric::Int(i) => i.to_string(),
            Numeric::Dec(d) => d.to_string(),
            Numeric::Flt(f) => exponential(f64::from(f32::from(f))),
            Numeric::Dbl(d) => exponential(f64::from(d)),
        }
    }

    /// The value as a typed literal in N-Triples form.
    pub(crate) fn to_term(self) -> Term {
        Term::Literal(format!("\"{}\"^^<{}>", self.lexical(), self.datatype()))
    }

    /// `op:numeric-add`. `None` on overflow (a SPARQL type error).
    pub(crate) fn add(self, other: Self) -> Option<Self> {
        match self.promote(other) {
            (Numeric::Int(a), Numeric::Int(b)) => a.checked_add(b).map(Numeric::Int),
            (Numeric::Dec(a), Numeric::Dec(b)) => a.checked_add(b).map(Numeric::Dec),
            (Numeric::Flt(a), Numeric::Flt(b)) => Some(Numeric::Flt(a + b)),
            (Numeric::Dbl(a), Numeric::Dbl(b)) => Some(Numeric::Dbl(a + b)),
            _ => unreachable!("promote returns two values of the same rank"),
        }
    }

    /// `op:numeric-subtract`.
    pub(crate) fn sub(self, other: Self) -> Option<Self> {
        match self.promote(other) {
            (Numeric::Int(a), Numeric::Int(b)) => a.checked_sub(b).map(Numeric::Int),
            (Numeric::Dec(a), Numeric::Dec(b)) => a.checked_sub(b).map(Numeric::Dec),
            (Numeric::Flt(a), Numeric::Flt(b)) => Some(Numeric::Flt(a - b)),
            (Numeric::Dbl(a), Numeric::Dbl(b)) => Some(Numeric::Dbl(a - b)),
            _ => unreachable!("promote returns two values of the same rank"),
        }
    }

    /// `op:numeric-multiply`.
    pub(crate) fn mul(self, other: Self) -> Option<Self> {
        match self.promote(other) {
            (Numeric::Int(a), Numeric::Int(b)) => a.checked_mul(b).map(Numeric::Int),
            (Numeric::Dec(a), Numeric::Dec(b)) => a.checked_mul(b).map(Numeric::Dec),
            (Numeric::Flt(a), Numeric::Flt(b)) => Some(Numeric::Flt(a * b)),
            (Numeric::Dbl(a), Numeric::Dbl(b)) => Some(Numeric::Dbl(a * b)),
            _ => unreachable!("promote returns two values of the same rank"),
        }
    }

    /// `op:numeric-divide`. Integer division yields `xsd:decimal`; dividing an
    /// integer or a decimal by zero is a type error, while float/double
    /// division by zero yields `INF`/`NaN` as IEEE 754 defines.
    pub(crate) fn div(self, other: Self) -> Option<Self> {
        match self.promote(other) {
            (Numeric::Int(a), Numeric::Int(b)) => Decimal::from(a)
                .checked_div(Decimal::from(b))
                .map(Numeric::Dec),
            (Numeric::Dec(a), Numeric::Dec(b)) => a.checked_div(b).map(Numeric::Dec),
            (Numeric::Flt(a), Numeric::Flt(b)) => Some(Numeric::Flt(a / b)),
            (Numeric::Dbl(a), Numeric::Dbl(b)) => Some(Numeric::Dbl(a / b)),
            _ => unreachable!("promote returns two values of the same rank"),
        }
    }

    /// `op:numeric-unary-minus`.
    pub(crate) fn neg(self) -> Option<Self> {
        Some(match self {
            Numeric::Int(i) => Numeric::Int(i.checked_neg()?),
            Numeric::Dec(d) => Numeric::Dec(d.checked_neg()?),
            Numeric::Flt(f) => Numeric::Flt(-f),
            Numeric::Dbl(d) => Numeric::Dbl(-d),
        })
    }

    /// `fn:abs`. The argument's type is preserved (§17.4.4.1).
    pub(crate) fn abs(self) -> Option<Self> {
        Some(match self {
            Numeric::Int(i) => Numeric::Int(i.checked_abs()?),
            Numeric::Dec(d) => Numeric::Dec(d.checked_abs()?),
            Numeric::Flt(f) => Numeric::Flt(f.abs()),
            Numeric::Dbl(d) => Numeric::Dbl(d.abs()),
        })
    }

    /// `fn:ceiling`. The argument's type is preserved — `CEIL("2.5"^^xsd:decimal)`
    /// is `xsd:decimal`, not `xsd:integer` (§17.4.4.2).
    pub(crate) fn ceil(self) -> Option<Self> {
        Some(match self {
            Numeric::Int(i) => Numeric::Int(i),
            Numeric::Dec(d) => Numeric::Dec(d.checked_ceil()?),
            Numeric::Flt(f) => Numeric::Flt(f.ceil()),
            Numeric::Dbl(d) => Numeric::Dbl(d.ceil()),
        })
    }

    /// `fn:floor` (§17.4.4.3), likewise type-preserving.
    pub(crate) fn floor(self) -> Option<Self> {
        Some(match self {
            Numeric::Int(i) => Numeric::Int(i),
            Numeric::Dec(d) => Numeric::Dec(d.checked_floor()?),
            Numeric::Flt(f) => Numeric::Flt(f.floor()),
            Numeric::Dbl(d) => Numeric::Dbl(d.floor()),
        })
    }

    /// `fn:round` (§17.4.4.4), likewise type-preserving. Rounds half toward
    /// positive infinity, so `ROUND(-2.5)` is `-2`.
    pub(crate) fn round(self) -> Option<Self> {
        Some(match self {
            Numeric::Int(i) => Numeric::Int(i),
            Numeric::Dec(d) => Numeric::Dec(d.checked_round()?),
            Numeric::Flt(f) => Numeric::Flt(f.round()),
            Numeric::Dbl(d) => Numeric::Dbl(d.round()),
        })
    }
}

/// The XSD canonical lexical form of a `float`/`double`: a mantissa with
/// exactly one digit before the point and at least one after it, then `E` and
/// the exponent (`2.0E-1`, `1.0E2`, `0.0E0`). Rust's `{:E}` already gives the
/// shortest round-tripping mantissa and a bare exponent; only the mandatory
/// fractional digit has to be added.
fn exponential(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v.is_sign_positive() { "INF" } else { "-INF" }.to_owned();
    }
    let formatted = format!("{v:E}");
    match formatted.split_once('E') {
        Some((mantissa, exponent)) if !mantissa.contains('.') => {
            format!("{mantissa}.0E{exponent}")
        }
        _ => formatted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(v: i64) -> Numeric {
        Numeric::from_i64(v)
    }

    fn parse(value: &str, local: &str) -> Numeric {
        Numeric::parse(value, &format!("{XSD}{local}")).expect("parses")
    }

    #[test]
    fn promotion_follows_the_operator_mapping() {
        // integer + integer stays integer; a decimal operand promotes both;
        // a double operand wins over everything.
        assert_eq!(int(1).add(int(2)).unwrap().lexical(), "3");
        assert_eq!(
            int(1).add(int(2)).unwrap().datatype(),
            format!("{XSD}integer")
        );
        let sum = parse("1.0", "decimal").add(int(2)).unwrap();
        assert_eq!(
            (sum.lexical(), sum.datatype()),
            ("3".into(), &*format!("{XSD}decimal"))
        );
        let sum = parse("2E-1", "double")
            .add(parse("0.2", "decimal"))
            .unwrap();
        assert_eq!(
            (sum.lexical(), sum.datatype()),
            ("4.0E-1".into(), &*format!("{XSD}double"))
        );
        // integer / integer is the one promotion the operator mapping forces.
        let quot = int(6).div(int(3)).unwrap();
        assert_eq!(
            (quot.lexical(), quot.datatype()),
            ("2".into(), &*format!("{XSD}decimal"))
        );
    }

    #[test]
    fn decimals_are_exact_not_f64() {
        let sum = parse("1.1", "decimal")
            .add(parse("2.2", "decimal"))
            .and_then(|n| n.add(parse("3.3", "decimal")))
            .and_then(|n| n.add(parse("4.5", "decimal")))
            .unwrap();
        assert_eq!(sum.lexical(), "11.1");
    }

    #[test]
    fn rounding_preserves_the_argument_type() {
        assert_eq!(
            parse("2.5", "decimal").ceil().unwrap().to_term(),
            Term::Literal(format!("\"3\"^^<{XSD}decimal>"))
        );
        assert_eq!(parse("-1.6", "decimal").floor().unwrap().lexical(), "-2");
        assert_eq!(parse("-2.5", "decimal").round().unwrap().lexical(), "-2");
        assert_eq!(
            int(-1).ceil().unwrap().to_term(),
            Term::Literal(format!("\"-1\"^^<{XSD}integer>"))
        );
    }

    #[test]
    fn doubles_render_in_canonical_form() {
        assert_eq!(parse("2E-1", "double").lexical(), "2.0E-1");
        assert_eq!(parse("32100", "double").lexical(), "3.21E4");
        assert_eq!(parse("0", "double").lexical(), "0.0E0");
    }

    #[test]
    fn non_numeric_datatypes_are_type_errors() {
        assert!(Numeric::parse("1", &format!("{XSD}string")).is_none());
        assert!(Numeric::parse("1", "http://example.org/nope").is_none());
        assert!(Numeric::parse("nope", &format!("{XSD}integer")).is_none());
        // Division by zero is an error for the exact types, INF for double.
        assert!(int(1).div(int(0)).is_none());
        assert_eq!(
            parse("1", "double")
                .div(parse("0", "double"))
                .unwrap()
                .lexical(),
            "INF"
        );
    }
}
