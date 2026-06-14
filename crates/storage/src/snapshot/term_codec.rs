//! Canonical kind-tagged byte encoding of terms for the snapshot dictionary.
//!
//! See the format spec in docs/plans/2026-06-14-SPEC-02-hdt-snapshot.md.

use super::varint::{read_uvarint, write_uvarint, zigzag_decode, zigzag_encode};
use crate::error::{Result, StorageError};
use oxrdf::{BlankNode, Literal, NamedNode, NamedNodeRef, Term};
use std::io::Cursor;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

const KIND_URI: u8 = 0x00;
const KIND_BLANK: u8 = 0x01;
const KIND_PLAIN: u8 = 0x02;
const KIND_LANG: u8 = 0x03;
const KIND_TYPED: u8 = 0x04;
const KIND_INLINE_INT: u8 = 0x05;
const KIND_TRIPLE: u8 = 0x06;
const KIND_DIR_LANG: u8 = 0x07;

fn snap_err(msg: impl Into<String>) -> StorageError {
    StorageError::Snapshot(msg.into())
}

/// Convert an `oxrdf::NamedOrBlankNode` into the equivalent `oxrdf::Term`. In oxrdf 0.3
/// `Subject` (= `NamedOrBlankNode`) only has IRI/blank-node variants; a triple
/// subject cannot itself be a triple term.
fn subject_to_term(subject: &oxrdf::NamedOrBlankNode) -> Term {
    match subject {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
    }
}

/// Encode a value-encoded integer (`TermKind::InlineInt`) to canonical bytes:
/// the `KIND_INLINE_INT` tag followed by the zigzag-encoded value.
pub fn encode_inline_int(buf: &mut Vec<u8>, value: i32) {
    buf.push(KIND_INLINE_INT);
    write_uvarint(buf, zigzag_encode(value)).expect("Vec write is infallible");
}

/// Encode a term to canonical bytes.
pub fn encode_term(buf: &mut Vec<u8>, term: &Term) {
    match term {
        Term::NamedNode(n) => {
            buf.push(KIND_URI);
            buf.extend_from_slice(n.as_str().as_bytes());
        }
        Term::BlankNode(b) => {
            buf.push(KIND_BLANK);
            buf.extend_from_slice(b.as_str().as_bytes());
        }
        Term::Literal(lit) => {
            if let Some(dir) = lit.direction() {
                // A directional language-tagged literal (RDF 1.2 rdf:dirLangString)
                // reports BOTH a language and a base direction, so check direction
                // first to avoid falling into the plain-lang branch and dropping it.
                let lang = lit
                    .language()
                    .expect("directional language literal always has a language tag");
                buf.push(KIND_DIR_LANG);
                buf.push(match dir {
                    oxrdf::BaseDirection::Ltr => 0u8,
                    oxrdf::BaseDirection::Rtl => 1u8,
                });
                write_uvarint(buf, lang.len() as u64).expect("Vec write is infallible");
                buf.extend_from_slice(lang.as_bytes());
                buf.extend_from_slice(lit.value().as_bytes());
            } else if let Some(lang) = lit.language() {
                buf.push(KIND_LANG);
                write_uvarint(buf, lang.len() as u64).expect("Vec write is infallible");
                buf.extend_from_slice(lang.as_bytes());
                buf.extend_from_slice(lit.value().as_bytes());
            } else if lit.datatype().as_str() == XSD_STRING {
                buf.push(KIND_PLAIN);
                buf.extend_from_slice(lit.value().as_bytes());
            } else {
                buf.push(KIND_TYPED);
                let dt = lit.datatype().as_str();
                write_uvarint(buf, dt.len() as u64).expect("Vec write is infallible");
                buf.extend_from_slice(dt.as_bytes());
                buf.extend_from_slice(lit.value().as_bytes());
            }
        }
        Term::Triple(t) => {
            buf.push(KIND_TRIPLE);
            let mut s = Vec::new();
            encode_term(&mut s, &subject_to_term(&t.subject));
            let mut p = Vec::new();
            encode_term(&mut p, &Term::NamedNode(t.predicate.clone()));
            write_uvarint(buf, s.len() as u64).expect("Vec write is infallible");
            buf.extend_from_slice(&s);
            write_uvarint(buf, p.len() as u64).expect("Vec write is infallible");
            buf.extend_from_slice(&p);
            encode_term(buf, &t.object.clone());
        }
    }
}

/// Decode canonical bytes back into a term. The whole slice is one term.
pub fn decode_term(bytes: &[u8]) -> Result<Term> {
    let (term, rest) = decode_term_prefix(bytes)?;
    if !rest.is_empty() {
        return Err(snap_err("trailing bytes after term"));
    }
    Ok(term)
}

/// Decode one term from the front of `bytes`, returning it and the unconsumed tail.
///
/// The variable-length kinds (URI/BLANK/PLAIN/LANG/DIR_LANG/TYPED) consume to the end of
/// their byte slice and are therefore only valid as the final or a
/// length-delimited field — which is why the s/p subterms in the triple-term
/// encoding carry explicit length prefixes.
fn decode_term_prefix(bytes: &[u8]) -> Result<(Term, &[u8])> {
    let (&kind, rest) = bytes
        .split_first()
        .ok_or_else(|| snap_err("empty term encoding"))?;
    match kind {
        KIND_URI => {
            let s = std::str::from_utf8(rest).map_err(|e| snap_err(e.to_string()))?;
            let n = NamedNode::new(s).map_err(|e| snap_err(e.to_string()))?;
            Ok((Term::NamedNode(n), &[]))
        }
        KIND_BLANK => {
            let s = std::str::from_utf8(rest).map_err(|e| snap_err(e.to_string()))?;
            let b = BlankNode::new(s).map_err(|e| snap_err(e.to_string()))?;
            Ok((Term::BlankNode(b), &[]))
        }
        KIND_PLAIN => {
            let s = std::str::from_utf8(rest).map_err(|e| snap_err(e.to_string()))?;
            Ok((Term::Literal(Literal::new_simple_literal(s)), &[]))
        }
        KIND_LANG => {
            let mut cur = Cursor::new(rest);
            let lang_len = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))? as usize;
            let body = &rest[cur.position() as usize..];
            if body.len() < lang_len {
                return Err(snap_err("lang literal truncated"));
            }
            let lang =
                std::str::from_utf8(&body[..lang_len]).map_err(|e| snap_err(e.to_string()))?;
            let value =
                std::str::from_utf8(&body[lang_len..]).map_err(|e| snap_err(e.to_string()))?;
            let lit = Literal::new_language_tagged_literal(value, lang)
                .map_err(|e| snap_err(e.to_string()))?;
            Ok((Term::Literal(lit), &[]))
        }
        // DirLang (0x07): [0x07][dir: u8 (0=Ltr, 1=Rtl)] ++ VByte(lang_len) ++
        // lang_utf8 ++ value_utf8. The value runs to the end of the slice.
        KIND_DIR_LANG => {
            let (&dir_byte, after_dir) = rest
                .split_first()
                .ok_or_else(|| snap_err("dir-lang literal missing direction byte"))?;
            let dir = match dir_byte {
                0 => oxrdf::BaseDirection::Ltr,
                1 => oxrdf::BaseDirection::Rtl,
                _ => return Err(snap_err("invalid base direction byte")),
            };
            let mut cur = Cursor::new(after_dir);
            let lang_len = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))? as usize;
            let body = &after_dir[cur.position() as usize..];
            if body.len() < lang_len {
                return Err(snap_err("dir-lang literal truncated"));
            }
            let lang =
                std::str::from_utf8(&body[..lang_len]).map_err(|e| snap_err(e.to_string()))?;
            let value =
                std::str::from_utf8(&body[lang_len..]).map_err(|e| snap_err(e.to_string()))?;
            let lit = Literal::new_directional_language_tagged_literal(value, lang, dir)
                .map_err(|e| snap_err(e.to_string()))?;
            Ok((Term::Literal(lit), &[]))
        }
        KIND_TYPED => {
            let mut cur = Cursor::new(rest);
            let dt_len = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))? as usize;
            let body = &rest[cur.position() as usize..];
            if body.len() < dt_len {
                return Err(snap_err("typed literal truncated"));
            }
            let dt = std::str::from_utf8(&body[..dt_len]).map_err(|e| snap_err(e.to_string()))?;
            let value =
                std::str::from_utf8(&body[dt_len..]).map_err(|e| snap_err(e.to_string()))?;
            let dt_node = NamedNode::new(dt).map_err(|e| snap_err(e.to_string()))?;
            Ok((
                Term::Literal(Literal::new_typed_literal(value, dt_node)),
                &[],
            ))
        }
        KIND_INLINE_INT => {
            let mut cur = Cursor::new(rest);
            let zz = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))?;
            let v = zigzag_decode(zz);
            Ok((
                Term::Literal(Literal::new_typed_literal(
                    v.to_string(),
                    NamedNodeRef::new(XSD_INTEGER).unwrap(),
                )),
                &rest[cur.position() as usize..],
            ))
        }
        KIND_TRIPLE => {
            let mut cur = Cursor::new(rest);
            let s_len = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))? as usize;
            let after_slen = cur.position() as usize;
            let s_end = after_slen
                .checked_add(s_len)
                .ok_or_else(|| snap_err("triple subject length overflow"))?;
            let s_bytes = rest
                .get(after_slen..s_end)
                .ok_or_else(|| snap_err("triple subject truncated"))?;
            let (s_term, _) = decode_term_prefix(s_bytes)?;
            let mut cur = Cursor::new(&rest[s_end..]);
            let p_len = read_uvarint(&mut cur).map_err(|e| snap_err(e.to_string()))? as usize;
            let p_off = s_end
                .checked_add(cur.position() as usize)
                .ok_or_else(|| snap_err("triple predicate offset overflow"))?;
            let p_end = p_off
                .checked_add(p_len)
                .ok_or_else(|| snap_err("triple predicate length overflow"))?;
            let p_bytes = rest
                .get(p_off..p_end)
                .ok_or_else(|| snap_err("triple predicate truncated"))?;
            let (p_term, _) = decode_term_prefix(p_bytes)?;
            let o_bytes = rest
                .get(p_end..)
                .ok_or_else(|| snap_err("triple object truncated"))?;
            let (o_term, _) = decode_term_prefix(o_bytes)?;
            let subject = match s_term {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                // oxrdf 0.3 `Subject` cannot hold a triple term or a literal.
                Term::Triple(_) => return Err(snap_err("triple-term in triple-term subject")),
                Term::Literal(_) => return Err(snap_err("literal in triple-term subject")),
            };
            let predicate = match p_term {
                Term::NamedNode(n) => n,
                _ => return Err(snap_err("non-IRI triple-term predicate")),
            };
            Ok((
                Term::Triple(Box::new(oxrdf::Triple {
                    subject,
                    predicate,
                    object: o_term,
                })),
                &[],
            ))
        }
        other => Err(snap_err(format!("unknown term kind tag {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(term: &Term) -> Term {
        let mut buf = Vec::new();
        encode_term(&mut buf, term);
        decode_term(&buf).unwrap()
    }

    #[test]
    fn round_trips_uri() {
        let t = Term::NamedNode(NamedNode::new("http://ex/a").unwrap());
        assert_eq!(rt(&t), t);
    }

    #[test]
    fn round_trips_blank_preserving_label() {
        let t = Term::BlankNode(BlankNode::new("b0").unwrap());
        assert_eq!(rt(&t), t);
    }

    #[test]
    fn round_trips_plain_lang_typed() {
        let plain = Term::Literal(Literal::new_simple_literal("hi"));
        let lang = Term::Literal(Literal::new_language_tagged_literal("bonjour", "fr").unwrap());
        let typed = Term::Literal(Literal::new_typed_literal(
            "3.14",
            NamedNode::new("http://www.w3.org/2001/XMLSchema#decimal").unwrap(),
        ));
        assert_eq!(rt(&plain), plain);
        assert_eq!(rt(&lang), lang);
        assert_eq!(rt(&typed), typed);
    }

    #[test]
    fn round_trips_directional_language_literal() {
        for dir in [oxrdf::BaseDirection::Ltr, oxrdf::BaseDirection::Rtl] {
            let lit = Literal::new_directional_language_tagged_literal("שלום", "he", dir).unwrap();
            let t = Term::Literal(lit);
            assert_eq!(rt(&t), t);
        }
    }

    #[test]
    fn plain_lang_literal_uses_kind_lang_not_dir_lang() {
        let mut buf = Vec::new();
        encode_term(
            &mut buf,
            &Term::Literal(Literal::new_language_tagged_literal("bonjour", "fr").unwrap()),
        );
        assert_eq!(buf[0], KIND_LANG);
    }

    #[test]
    fn invalid_direction_byte_errors() {
        // KIND_DIR_LANG with an invalid direction byte (2).
        let mut buf = vec![KIND_DIR_LANG, 2u8];
        write_uvarint(&mut buf, 2).unwrap();
        buf.extend_from_slice(b"hevalue");
        assert!(decode_term(&buf).is_err());
    }

    #[test]
    fn inline_int_encodes_as_canonical_integer() {
        let mut buf = Vec::new();
        encode_inline_int(&mut buf, -42);
        assert_eq!(buf[0], KIND_INLINE_INT);
        let decoded = decode_term(&buf).unwrap();
        assert_eq!(
            decoded,
            Term::Literal(Literal::new_typed_literal(
                "-42",
                NamedNodeRef::new(XSD_INTEGER).unwrap()
            ))
        );
    }

    #[test]
    fn round_trips_nested_triple_term() {
        let inner = oxrdf::Triple {
            subject: oxrdf::NamedOrBlankNode::NamedNode(NamedNode::new("http://s").unwrap()),
            predicate: NamedNode::new("http://p").unwrap(),
            object: Term::Literal(Literal::new_simple_literal("o")),
        };
        let t = Term::Triple(Box::new(inner));
        assert_eq!(rt(&t), t);
    }

    #[test]
    fn empty_encoding_errors() {
        assert!(decode_term(&[]).is_err());
    }

    #[test]
    fn truncated_lang_literal_errors() {
        // KIND_LANG with a lang_len varint (= 10) larger than the remaining bytes.
        let mut buf = vec![KIND_LANG];
        write_uvarint(&mut buf, 10).unwrap();
        buf.extend_from_slice(b"fr"); // only 2 bytes follow, not 10
        assert!(decode_term(&buf).is_err());
    }

    #[test]
    fn truncated_typed_literal_errors() {
        // KIND_TYPED with a dt_len varint (= 20) larger than the remaining bytes.
        let mut buf = vec![KIND_TYPED];
        write_uvarint(&mut buf, 20).unwrap();
        buf.extend_from_slice(b"http://x"); // far fewer than 20 bytes
        assert!(decode_term(&buf).is_err());
    }

    #[test]
    fn unknown_kind_tag_errors() {
        assert!(decode_term(&[0xFF]).is_err());
    }

    #[test]
    fn triple_term_subject_overflow_does_not_panic() {
        // KIND_TRIPLE with an s_len varint that overruns the buffer. Must return
        // Err (not panic) in both debug and release — exercises the checked-arith path.
        let mut buf = vec![KIND_TRIPLE];
        write_uvarint(&mut buf, u64::MAX).unwrap();
        buf.extend_from_slice(b"junk");
        assert!(decode_term(&buf).is_err());
    }
}
