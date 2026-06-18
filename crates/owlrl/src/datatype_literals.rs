//! OWL 2 RL literal-value datatype rules: `dt-eq`, `dt-diff`, `dt-not-type`.
//!
//! Unlike [`crate::datatypes`] (which reasons over datatype *IRIs* — the
//! `dt-type1`/`dt-type2` declarations), this module reasons over the **values**
//! that literals denote. The three OWL 2 RL/RDF datatype rules it implements
//! (W3C OWL 2 Profiles, "Reasoning … using Rules", datatype rules table):
//!
//! - **`dt-eq`** — two literals whose lexical forms map to the *same* value in
//!   the value space are `owl:sameAs`. This is the cross-lexical case:
//!   `"1"^^xsd:integer` ≡ `"+1"^^xsd:integer` ≡ `"01"^^xsd:integer`, and across
//!   the numeric tower `"1"^^xsd:integer` ≡ `"1"^^xsd:byte`.
//! - **`dt-diff`** — two literals whose lexical forms map to *different* values
//!   (within a comparable value space) are `owl:differentFrom`. Distinct
//!   strings (`"Peter"` vs `"Kichwa-Tembo"`) and distinct numbers
//!   (`"1"` vs `"2"`) are different.
//! - **`dt-not-type`** — a literal whose lexical form is **not** in the value
//!   space of its datatype is ill-typed; the OWL 2 RL profile concludes a
//!   global inconsistency (`owl:Nothing` membership) for the offending term.
//!
//! ## Why this lives outside `rules.toml`
//! The compiled rule engine matches purely on `TermId`s; the datatype and
//! parsed value behind a literal `TermId` are invisible to it. These rules
//! need to *parse* a literal's lexical form against its datatype's value space,
//! so they run as a load-time pass (`integration.rs`) that has the dictionary
//! (`TermId → lexical key`) in hand — the same shape as the
//! `resolve_max_card_restrictions` cardinality-literal pass.
//!
//! ## Stage-1 scope
//! The value space covered is the **Stage-1 datatype set** (see
//! [`crate::datatypes::XSD_DATATYPES`]): the XSD numeric integer tower
//! (`xsd:integer` and its sub/peer integer types), `xsd:decimal`, `xsd:string`,
//! `xsd:boolean`, plus plain (`rdf:langString` / `xsd:string`) literals. Values
//! that this module cannot place into a canonical value-space class (e.g.
//! `xsd:dateTime`, user datatypes) are treated as **opaque**: two such literals
//! are equal iff their lexical keys are byte-identical, and never
//! cross-compared with a different value space. That keeps the rules sound
//! (no false `sameAs`/`differentFrom`) while deferring full value-space
//! coverage to Stage 2.

/// A literal decomposed into `(lexical-value, datatype-IRI, language-tag)`.
///
/// `language` is `Some` only for language-tagged literals (`"x"@en`), in which
/// case `datatype` is the conventional `rdf:langString`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedLiteral<'a> {
    pub value: &'a str,
    pub datatype: &'a str,
    pub language: Option<&'a str>,
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

/// Parse a dictionary literal key of the form `"value"^^<datatype>` or
/// `"value"@lang` (see `integration::intern_literal`) back into its parts.
///
/// Returns `None` for keys that are not literals (IRIs, blank nodes — those
/// never start with `"`). The lexical value may itself contain escaped quotes;
/// the typed form is split on the *last* `"^^<` so an embedded `"^^<` in the
/// value does not mis-parse (literal values from oxrdf are already unescaped,
/// but the split is defensive).
pub fn parse_literal_key(key: &str) -> Option<ParsedLiteral<'_>> {
    let rest = key.strip_prefix('"')?;
    if let Some(close) = rest.rfind("\"^^<") {
        let value = &rest[..close];
        let datatype = rest[close + 4..].strip_suffix('>')?;
        Some(ParsedLiteral {
            value,
            datatype,
            language: None,
        })
    } else if let Some(close) = rest.rfind("\"@") {
        let value = &rest[..close];
        let language = &rest[close + 2..];
        Some(ParsedLiteral {
            value,
            datatype: RDF_LANG_STRING,
            language: Some(language),
        })
    } else {
        None
    }
}

/// A value-space class: two literals are `dt-eq` iff they share a `ValueClass`,
/// and `dt-diff` iff they have *comparable* but unequal `ValueClass`es.
///
/// Comparability is encoded by the variant: only two `Integer`s, two
/// `Boolean`s, or two `Plain`/`Opaque` of matching shape are compared. We never
/// declare a `String` and an `Integer` `differentFrom` each other — they live
/// in disjoint value spaces, and OWL 2 RL only concludes `differentFrom` for
/// literals known to denote distinct values *of a comparable kind*. (Declaring
/// every cross-space pair `differentFrom` is also sound but needlessly
/// quadratic and not required by the profile's intent for these rules.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ValueClass {
    /// An exact integer value (canonicalised across the whole signed integer
    /// tower and `xsd:decimal` integers). Carries the canonical decimal string.
    Integer(String),
    /// An `xsd:boolean` value.
    Boolean(bool),
    /// A plain string value: `xsd:string`, `rdf:langString` (with its language
    /// tag folded in so `"x"@en` ≠ `"x"@fr`), or an `xsd:decimal`/`xsd:double`
    /// kept as its lexical form. Compared byte-for-byte.
    Plain(String),
}

/// `xsd:` integer-tower datatypes whose value space is the integers. A literal
/// typed with one of these is parsed as an `i128`; cross-type integers compare
/// by value (`dt-eq`: `"1"^^xsd:byte` ≡ `"1"^^xsd:integer`).
const XSD_INTEGER_DATATYPES: &[&str] = &[
    "integer",
    "long",
    "int",
    "short",
    "byte",
    "nonNegativeInteger",
    "positiveInteger",
    "unsignedLong",
    "unsignedInt",
    "unsignedShort",
    "unsignedByte",
    "nonPositiveInteger",
    "negativeInteger",
];

/// Strip the XSD namespace prefix; returns the local name iff `dt` is in the
/// `xsd:` namespace.
fn xsd_local(dt: &str) -> Option<&str> {
    dt.strip_prefix(XSD)
}

/// Classify a parsed literal into a [`ValueClass`], or report that it is
/// **ill-typed** — its lexical form is not in its datatype's value space
/// (`dt-not-type`).
///
/// `Ok(Some(class))` — well-typed, placed into a value class.
/// `Ok(None)` — well-typed but outside the Stage-1 comparable value spaces
///   (opaque; only byte-identical lexical keys are `dt-eq`, handled by the
///   caller via key equality, never `dt-diff`).
/// `Err(())` — ill-typed (`dt-not-type`): a global inconsistency.
#[allow(clippy::result_unit_err)]
pub fn classify(lit: &ParsedLiteral<'_>) -> Result<Option<ValueClass>, ()> {
    // Language-tagged literals are plain strings keyed by (value, language).
    if let Some(lang) = lit.language {
        return Ok(Some(ValueClass::Plain(format!(
            "@{lang}\u{1}{}",
            lit.value
        ))));
    }
    let Some(local) = xsd_local(lit.datatype) else {
        // Non-XSD (user) datatype: opaque. Cannot validate or compare values.
        return Ok(None);
    };
    if XSD_INTEGER_DATATYPES.contains(&local) {
        // Integer value space: must parse as an integer, and (for the bounded
        // sub-types) fall within the type's range. A lexical form that is not
        // a valid integer of this type is ill-typed (`dt-not-type`).
        let Some(canon) = parse_xsd_integer(local, lit.value) else {
            return Err(());
        };
        return Ok(Some(ValueClass::Integer(canon)));
    }
    match local {
        "string" => Ok(Some(ValueClass::Plain(format!("s\u{1}{}", lit.value)))),
        "boolean" => match lit.value {
            "true" | "1" => Ok(Some(ValueClass::Boolean(true))),
            "false" | "0" => Ok(Some(ValueClass::Boolean(false))),
            // A lexical form outside {true,false,1,0} is not in xsd:boolean's
            // value space.
            _ => Err(()),
        },
        // decimal / double / float / dateTime / anyURI / … : Stage-1 keeps the
        // lexical form opaque per-datatype (no cross-lexical canonicalisation,
        // no range validation). Two such literals are dt-eq only when their
        // whole keys match, which the caller handles before classification.
        _ => Ok(None),
    }
}

/// Parse and validate an XSD integer-tower lexical form, returning the
/// canonical decimal string (no leading `+`, no leading zeros, `-0` → `0`) on
/// success, or `None` if the lexical form is not a valid value of `local`.
///
/// The **unbounded** integer types (`xsd:integer`, `xsd:nonNegativeInteger`,
/// `xsd:positiveInteger`, `xsd:nonPositiveInteger`, `xsd:negativeInteger`) are
/// validated by *string* canonicalisation + sign, never by fixed-width parsing
/// — a 40-digit `xsd:integer` is a perfectly valid value and must not be
/// rejected (which would inject a false `owl:Nothing`). The **bounded**
/// sub-types (`long`/`int`/`short`/`byte` and the `unsigned*` family) do carry
/// a finite value space, so they parse into the corresponding fixed-width type:
/// a lexical value outside the range is genuinely ill-typed.
fn parse_xsd_integer(local: &str, value: &str) -> Option<String> {
    // Canonicalise the lexical form (validates integer syntax, arbitrary
    // precision). Returns the canonical string and whether it is negative.
    let (canon, negative) = canonicalize_integer(value)?;
    let is_zero = canon == "0";
    match local {
        // Unbounded: only the sign constraint matters.
        "integer" => Some(canon),
        "nonNegativeInteger" => (!negative).then_some(canon),
        "positiveInteger" => (!negative && !is_zero).then_some(canon),
        "nonPositiveInteger" => (negative || is_zero).then_some(canon),
        "negativeInteger" => (negative && !is_zero).then_some(canon),
        // Bounded signed tower: parse into the fixed-width type. Overflow ⇒
        // out of value space ⇒ ill-typed.
        "long" => value.parse::<i64>().ok().map(|n| n.to_string()),
        "int" => value.parse::<i32>().ok().map(|n| n.to_string()),
        "short" => value.parse::<i16>().ok().map(|n| n.to_string()),
        "byte" => value.parse::<i8>().ok().map(|n| n.to_string()),
        // Bounded unsigned tower.
        "unsignedLong" => value.parse::<u64>().ok().map(|n| n.to_string()),
        "unsignedInt" => value.parse::<u32>().ok().map(|n| n.to_string()),
        "unsignedShort" => value.parse::<u16>().ok().map(|n| n.to_string()),
        "unsignedByte" => value.parse::<u8>().ok().map(|n| n.to_string()),
        _ => Some(canon),
    }
}

/// Validate an XSD integer lexical form by string and return its canonical
/// decimal form (no leading `+`, no insignificant leading zeros, `-0` → `0`),
/// plus whether it is negative. Arbitrary precision — does not parse into a
/// fixed-width integer, so values beyond `i128` are still accepted.
///
/// `None` if the lexical form is not a syntactically valid integer (empty,
/// non-digit characters, a lone sign, etc.) — that is the `dt-not-type` case.
fn canonicalize_integer(value: &str) -> Option<(String, bool)> {
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Strip insignificant leading zeros; an all-zero string canonicalises to
    // "0" (and loses its sign: -0 == 0).
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        return Some(("0".to_string(), false));
    }
    let canon = if negative {
        format!("-{trimmed}")
    } else {
        trimmed.to_string()
    };
    Some((canon, negative))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(value: &str, dt_local: &str) -> String {
        format!("\"{value}\"^^<{XSD}{dt_local}>")
    }

    #[test]
    fn parse_typed_and_lang() {
        let k = typed("42", "integer");
        let p = parse_literal_key(&k).unwrap();
        assert_eq!(p.value, "42");
        assert_eq!(p.datatype, format!("{XSD}integer"));
        assert_eq!(p.language, None);

        let p = parse_literal_key("\"hi\"@en").unwrap();
        assert_eq!(p.value, "hi");
        assert_eq!(p.language, Some("en"));
        assert_eq!(p.datatype, RDF_LANG_STRING);

        assert!(parse_literal_key("http://ex/iri").is_none());
        assert!(parse_literal_key("_:b0").is_none());
    }

    #[test]
    fn integer_cross_lexical_equality() {
        // 1 ≡ +1 ≡ 01, all xsd:integer.
        let a = classify(&parse_literal_key(&typed("1", "integer")).unwrap())
            .unwrap()
            .unwrap();
        let b = classify(&parse_literal_key(&typed("+1", "integer")).unwrap())
            .unwrap()
            .unwrap();
        let c = classify(&parse_literal_key(&typed("01", "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn integer_cross_datatype_equality() {
        // 1^^xsd:byte ≡ 1^^xsd:integer (same value space point).
        let a = classify(&parse_literal_key(&typed("1", "byte")).unwrap())
            .unwrap()
            .unwrap();
        let b = classify(&parse_literal_key(&typed("1", "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_integers_differ() {
        let a = classify(&parse_literal_key(&typed("1", "integer")).unwrap())
            .unwrap()
            .unwrap();
        let b = classify(&parse_literal_key(&typed("2", "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn negative_zero_canonicalises() {
        let a = classify(&parse_literal_key(&typed("-0", "integer")).unwrap())
            .unwrap()
            .unwrap();
        let b = classify(&parse_literal_key(&typed("0", "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn large_unbounded_integer_is_well_typed() {
        // A value far beyond i128 must NOT be treated as ill-typed —
        // xsd:integer is unbounded. (Regression for the i128-overflow
        // false-inconsistency.)
        let big = "123456789012345678901234567890123456789012345678901234567890";
        assert!(
            classify(&parse_literal_key(&typed(big, "integer")).unwrap()).is_ok(),
            "a 60-digit xsd:integer is a valid value, not dt-not-type"
        );
        // And two equal big integers (one with leading zeros) are dt-eq.
        let a = classify(&parse_literal_key(&typed(big, "integer")).unwrap())
            .unwrap()
            .unwrap();
        let leading_zeros = format!("00{big}");
        let b = classify(&parse_literal_key(&typed(&leading_zeros, "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(a, b, "leading zeros do not change the canonical value");
        // Sign constraints still apply to the unbounded subtypes.
        let neg_big = format!("-{big}");
        assert!(
            classify(&parse_literal_key(&typed(&neg_big, "nonNegativeInteger")).unwrap()).is_err(),
            "a large negative value is still out of nonNegativeInteger's space"
        );
        assert!(
            classify(&parse_literal_key(&typed(&neg_big, "nonPositiveInteger")).unwrap()).is_ok(),
            "a large negative value is in nonPositiveInteger's space"
        );
    }

    #[test]
    fn malformed_integer_lexical_is_ill_typed() {
        for bad in ["", "+", "-", " 1", "1 ", "1.0", "0x1", "1_000", "abc"] {
            assert!(
                classify(&parse_literal_key(&typed(bad, "integer")).unwrap()).is_err(),
                "{bad:?} is not a valid xsd:integer lexical form"
            );
        }
    }

    #[test]
    fn distinct_strings_differ() {
        let a = classify(&parse_literal_key(&typed("Peter", "string")).unwrap())
            .unwrap()
            .unwrap();
        let b = classify(&parse_literal_key(&typed("Kichwa-Tembo", "string")).unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn string_and_integer_are_incomparable() {
        // Same lexical "1", different value spaces → different ValueClass
        // variants, never dt-eq and (by the caller's variant-guard) never
        // dt-diff.
        let s = classify(&parse_literal_key(&typed("1", "string")).unwrap())
            .unwrap()
            .unwrap();
        let i = classify(&parse_literal_key(&typed("1", "integer")).unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(s, i);
        assert!(matches!(s, ValueClass::Plain(_)));
        assert!(matches!(i, ValueClass::Integer(_)));
    }

    #[test]
    fn lang_tag_distinguishes_values() {
        let en = classify(&parse_literal_key("\"x\"@en").unwrap())
            .unwrap()
            .unwrap();
        let fr = classify(&parse_literal_key("\"x\"@fr").unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(en, fr);
    }

    #[test]
    fn boolean_lexical_variants() {
        let t1 = classify(&parse_literal_key(&typed("true", "boolean")).unwrap())
            .unwrap()
            .unwrap();
        let t2 = classify(&parse_literal_key(&typed("1", "boolean")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(t1, t2);
        let f = classify(&parse_literal_key(&typed("false", "boolean")).unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(t1, f);
    }

    #[test]
    fn ill_typed_integer_is_dt_not_type() {
        // "abc" is not an integer.
        assert!(classify(&parse_literal_key(&typed("abc", "integer")).unwrap()).is_err());
        // 999 is out of xsd:byte range [-128,127].
        assert!(classify(&parse_literal_key(&typed("999", "byte")).unwrap()).is_err());
        // -1 is not a nonNegativeInteger.
        assert!(classify(&parse_literal_key(&typed("-1", "nonNegativeInteger")).unwrap()).is_err());
        // 0 is not a positiveInteger.
        assert!(classify(&parse_literal_key(&typed("0", "positiveInteger")).unwrap()).is_err());
    }

    #[test]
    fn ill_typed_boolean_is_dt_not_type() {
        assert!(classify(&parse_literal_key(&typed("maybe", "boolean")).unwrap()).is_err());
    }

    #[test]
    fn user_datatype_is_opaque() {
        let k = "\"x\"^^<http://example.org/myType>";
        assert_eq!(classify(&parse_literal_key(k).unwrap()).unwrap(), None);
    }

    #[test]
    fn well_typed_in_range_subtypes() {
        // Boundary values that ARE in range must classify Ok.
        assert!(classify(&parse_literal_key(&typed("127", "byte")).unwrap()).is_ok());
        assert!(classify(&parse_literal_key(&typed("-128", "byte")).unwrap()).is_ok());
        assert!(classify(&parse_literal_key(&typed("255", "unsignedByte")).unwrap()).is_ok());
        assert!(classify(&parse_literal_key(&typed("1", "positiveInteger")).unwrap()).is_ok());
    }
}
