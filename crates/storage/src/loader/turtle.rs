//! Turtle bulk loader — streaming, and parallel-chunked behind an opt-in.
//!
//! Uses `oxttl::TurtleParser`. Turtle carries no graph component, so every
//! triple lands in the default graph (SPEC-02 F7's reserved sentinel).
//! Prefixes, `a`, collections, and blank-node property lists are expanded by
//! the parser before they reach the dictionary.
//!
//! Relative IRIs resolve against a base IRI. [`load_turtle_file`] derives a
//! best-effort `file://` base from the document path (the conventional RDF
//! base), so files that use document-relative IRIs load. [`load_turtle_reader`]
//! has no inherent base and parses base-less (relative IRIs error);
//! [`load_turtle_reader_with_base`] lets a caller supply one explicitly.
//!
//! # Parallel chunking is opt-in
//!
//! Unlike N-Triples, splitting Turtle is not unconditionally safe. `oxttl`
//! documents its Turtle chunker as able to "fail or return wrong results if
//! there are prefixes or base iris that are not defined at the top of the
//! document, or valid turtle syntax inside literal values". The chunker
//! collects the prefixes in force before the document's *first* triple and
//! copies them into every chunk parser; a `@prefix` or `@base` declared later
//! is invisible to the chunks after it.
//!
//! There is a second gap the docs do not mention: the chunker copies the
//! leading prefixes into each chunk parser but **not** the base IRI. A base
//! the caller passes in survives (it is set before the split); a `@base`
//! *directive* does not, even a leading one.
//!
//! So the parallel path is reached only by asking for it —
//! [`load_turtle_slice`], [`for_each_turtle_batch`], or setting **both**
//! `HORNDB_PARALLEL_TURTLE=1` and `HORNDB_LOAD_THREADS` for
//! [`load_turtle_file`]. Two knobs, because Turtle's split carries a soundness
//! caveat that the line-based N-Triples/N-Quads split does not. Even then
//! [`turtle_split_is_safe`] must clear the document; a rejected one falls back
//! to a serial parse of the same bytes.
//!
//! The residual risk `oxttl` names — a literal that itself contains three
//! parseable triples, which could fool the chunker's boundary heuristic — is
//! not detectable without parsing, and is the reason the opt-in exists.

use crate::error::{Result, StorageError};
use crate::loader::parallel::{
    load_threads, parse_chunks_ordered, should_read_whole_file, slice_threads,
};
use crate::loader::{load_quads, subject_to_term, LoadStats, QuadSink, SinkTimer};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::{Term, Triple};
use oxttl::TurtleParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Load a Turtle file. Serial unless `HORNDB_PARALLEL_TURTLE=1` is set — the
/// [`load_threads`] default going threaded (HDB-96) does not reach here,
/// because splitting Turtle carries a soundness caveat the line-based formats
/// do not. With the opt-in it takes the parallel-chunked path when the file is
/// large enough and [`turtle_split_is_safe`] clears it.
pub fn load_turtle_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    // Best-effort document base so relative IRIs resolve against the file's own
    // location. Drop it if it does not form a valid base IRI (rather than
    // failing the import), leaving base-less parsing for that pathological path.
    let base = file_base_iri(path).filter(|b| TurtleParser::new().with_base_iri(b).is_ok());
    let parallel_opt_in = matches!(
        std::env::var("HORNDB_PARALLEL_TURTLE").as_deref(),
        Ok("1") | Ok("true")
    );
    let mut stats = if parallel_opt_in && should_read_whole_file(bytes, load_threads()) {
        drop(file);
        load_turtle_slice(store, &std::fs::read(path)?, base.as_deref())?
    } else {
        let reader = BufReader::with_capacity(1 << 20, file);
        load_turtle_reader_with_base(store, reader, base.as_deref())?
    };
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_turtle_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    load_turtle_reader_with_base(store, reader, None)
}

/// Load Turtle with an explicit base IRI for relative-IRI resolution. An
/// invalid `base_iri` is a hard error (unlike the best-effort path base used by
/// [`load_turtle_file`]); pass `None` to parse base-less.
pub fn load_turtle_reader_with_base<R: Read>(
    store: &Store,
    reader: R,
    base_iri: Option<&str>,
) -> Result<LoadStats> {
    let parser = turtle_parser(base_iri)?;
    load_quads(
        store,
        parser.for_reader(reader).map(|t| {
            let triple = t.map_err(|e| StorageError::TurtleParse(format!("{e}")))?;
            Ok((
                DEFAULT_GRAPH,
                subject_to_term(triple.subject),
                Term::NamedNode(triple.predicate),
                triple.object,
            ))
        }),
    )
}

/// Load an in-memory Turtle document, parsing on [`load_threads`] threads when
/// [`turtle_split_is_safe`] clears the document and it is large enough;
/// otherwise parses the same bytes serially.
///
/// Interning stays on the calling thread in document order, so a document that
/// takes the parallel path ends up with the same triples, dictionary contents,
/// and term ids as the serial path.
pub fn load_turtle_slice(store: &Store, bytes: &[u8], base_iri: Option<&str>) -> Result<LoadStats> {
    load_turtle_slice_with_threads(store, bytes, base_iri, slice_threads(bytes.len()))
}

/// [`load_turtle_slice`] with an explicit parse-thread count. `threads <= 1`
/// parses serially; anything higher splits when — and only when —
/// [`turtle_split_is_safe`] clears the document.
pub fn load_turtle_slice_with_threads(
    store: &Store,
    bytes: &[u8],
    base_iri: Option<&str>,
    threads: usize,
) -> Result<LoadStats> {
    let mut sink = QuadSink::new(store);
    let mut timer = SinkTimer::new();
    for_each_turtle_batch(bytes, base_iri, threads, |triples| {
        timer.sink(|| {
            for t in triples {
                sink.push(
                    DEFAULT_GRAPH,
                    &subject_to_term(t.subject),
                    &Term::NamedNode(t.predicate),
                    &t.object,
                )?;
            }
            Ok(())
        })
    })?;
    timer.record_parse(sink.total);
    sink.finish()
}

/// Parse an in-memory Turtle document on `threads` threads, handing `sink`
/// batches of triples **in document order**.
///
/// Falls back to a single serial parser when `threads <= 1` or when
/// [`turtle_split_is_safe`] rejects the document. The fallback parses the same
/// bytes, so the caller sees the same triples either way. No size floor is
/// applied here (see [`load_turtle_slice`] for that).
pub fn for_each_turtle_batch<F>(
    bytes: &[u8],
    base_iri: Option<&str>,
    threads: usize,
    sink: F,
) -> Result<()>
where
    F: FnMut(Vec<Triple>) -> Result<()>,
{
    let parser = turtle_parser(base_iri)?;
    let parallel = threads > 1 && turtle_split_is_safe(bytes);
    let parsers = if parallel {
        parser.split_slice_for_parallel_parsing(bytes, threads)
    } else {
        vec![parser.for_slice(bytes)]
    };
    let chunks = parsers
        .into_iter()
        .map(|p| {
            Box::new(p.map(|t| t.map_err(|e| StorageError::TurtleParse(format!("{e}")))))
                as Box<dyn Iterator<Item = Result<Triple>> + Send + '_>
        })
        .collect();
    parse_chunks_ordered(chunks, sink)
}

fn turtle_parser(base_iri: Option<&str>) -> Result<TurtleParser> {
    let mut parser = TurtleParser::new();
    if let Some(base) = base_iri {
        parser = parser
            .with_base_iri(base)
            .map_err(|e| StorageError::TurtleParse(format!("invalid base IRI {base:?}: {e}")))?;
    }
    Ok(parser)
}

/// Can this Turtle document be split into independently parsed chunks?
///
/// Two things have to hold:
///
/// 1. **No `@base` / `BASE` anywhere.** `oxttl`'s chunker re-injects the
///    document's leading `@prefix` declarations into every chunk parser, but
///    nothing carries the base IRI across. A chunk that starts after a `@base`
///    would resolve relative IRIs against the wrong base — or fail with "no
///    scheme found in an absolute IRI" when the caller supplied no base at
///    all. A base the *caller* passes in is fine: it is set on the parser
///    before the split, so every chunk inherits it.
/// 2. **No `@prefix` / `PREFIX` after the leading directive block.** The
///    chunker collects the prefixes in force before the document's first
///    triple; one declared later is invisible to the chunks that follow it.
///
/// The scan is one pass over the bytes and is deliberately biased towards
/// "unsafe": it steps over comments, IRIs, and short/long string literals so a
/// directive-looking sequence inside one does not count, but anything it is
/// unsure about is reported as unsafe. A false "unsafe" costs a serial parse;
/// a false "safe" would corrupt the load.
pub fn turtle_split_is_safe(bytes: &[u8]) -> bool {
    let leading_end = leading_directive_block_end(bytes);
    !scan_directives(bytes)
        .into_iter()
        .any(|(pos, kind)| kind == Directive::Base || pos >= leading_end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Directive {
    Prefix,
    Base,
}

/// Byte offset just past the document's leading run of directives.
fn leading_directive_block_end(bytes: &[u8]) -> usize {
    let mut i = 0;
    loop {
        i = skip_ws_and_comments(bytes, i);
        if i >= bytes.len() {
            return i;
        }
        let rest = &bytes[i..];
        let next = if rest.starts_with(b"@prefix") || rest.starts_with(b"@base") {
            // `@prefix pn: <iri> .` / `@base <iri> .` — the IRI first (its own
            // bytes may contain `.`), then the terminating dot.
            skip_iri_then_dot(bytes, i)
        } else if sparql_directive(rest).is_some() {
            // `PREFIX pn: <iri>` / `BASE <iri>` — no terminating dot.
            skip_to_iri_end(bytes, i)
        } else {
            return i;
        };
        match next {
            Some(n) => i = n,
            // Malformed directive: end the block here. `scan_directives` then
            // reports it at an offset >= the block end, so the document is
            // called unsafe.
            None => return i,
        }
    }
}

/// SPARQL-style `PREFIX` / `BASE` keyword at the head of `rest`, if one starts
/// there. Requires the keyword to be followed by whitespace or `<` so a
/// prefixed name such as `basement:x` does not match.
fn sparql_directive(rest: &[u8]) -> Option<Directive> {
    for (kw, kind) in [
        (b"prefix".as_slice(), Directive::Prefix),
        (b"base".as_slice(), Directive::Base),
    ] {
        if rest.len() >= kw.len() && rest[..kw.len()].eq_ignore_ascii_case(kw) {
            match rest.get(kw.len()) {
                Some(c) if c.is_ascii_whitespace() || *c == b'<' => return Some(kind),
                _ => {}
            }
        }
    }
    None
}

fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        return i;
    }
}

/// From a directive keyword at `i`, skip past the directive's IRI.
fn skip_to_iri_end(bytes: &[u8], i: usize) -> Option<usize> {
    let open = bytes[i..].iter().position(|&b| b == b'<')? + i;
    let end = skip_iri(bytes, open);
    (bytes.get(end.wrapping_sub(1)) == Some(&b'>')).then_some(end)
}

/// As [`skip_to_iri_end`], then past the `.` that terminates an `@`-form
/// directive.
fn skip_iri_then_dot(bytes: &[u8], i: usize) -> Option<usize> {
    let after_iri = skip_to_iri_end(bytes, i)?;
    let dot = skip_ws_and_comments(bytes, after_iri);
    (bytes.get(dot) == Some(&b'.')).then_some(dot + 1)
}

/// Every `@prefix` / `@base` / `PREFIX` / `BASE` token in the document, with
/// its byte offset, skipping comments, IRIs, and string literals.
fn scan_directives(bytes: &[u8]) -> Vec<(usize, Directive)> {
    let mut found = Vec::new();
    let mut i = 0;
    // A language tag (`"x"@en`) is the one place `@` legitimately follows a
    // token with no separator, so only suppress the `@` check right after a
    // literal.
    let mut after_literal = false;
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                after_literal = false;
            }
            b'<' => {
                i = skip_iri(bytes, i);
                after_literal = false;
            }
            b'"' | b'\'' => {
                i = skip_literal(bytes, i);
                after_literal = true;
            }
            b'@' => {
                if !after_literal {
                    let rest = &bytes[i..];
                    if rest.starts_with(b"@prefix") {
                        found.push((i, Directive::Prefix));
                    } else if rest.starts_with(b"@base") {
                        found.push((i, Directive::Base));
                    }
                }
                i += 1;
                after_literal = false;
            }
            b'p' | b'P' | b'b' | b'B' => {
                let preceded_by_name = i > 0 && is_name_byte(bytes[i - 1]);
                if !preceded_by_name {
                    if let Some(kind) = sparql_directive(&bytes[i..]) {
                        found.push((i, kind));
                    }
                }
                // Step over the whole word so its interior cannot re-trigger.
                i += 1;
                while i < bytes.len() && is_name_byte(bytes[i]) {
                    i += 1;
                }
                after_literal = false;
            }
            _ => {
                i += 1;
                after_literal = false;
            }
        }
    }
    found
}

/// Bytes that can continue a name-like token; used only to tell a keyword from
/// the middle of a longer word.
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b':' || b >= 0x80
}

/// Offset just past the `>` closing the IRI that starts at `i`.
fn skip_iri(bytes: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'>' => return j + 1,
            b'\n' => return j, // unterminated: not a valid IRI, stop here
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Offset just past the string literal that starts at `i` (`"`, `'`, `"""`, or
/// `'''`), honouring `\` escapes.
fn skip_literal(bytes: &[u8], i: usize) -> usize {
    let q = bytes[i];
    let long = bytes.get(i + 1) == Some(&q) && bytes.get(i + 2) == Some(&q);
    let mut j = if long { i + 3 } else { i + 1 };
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            c if c == q => {
                if !long {
                    return j + 1;
                }
                if bytes.get(j + 1) == Some(&q) && bytes.get(j + 2) == Some(&q) {
                    return j + 3;
                }
                j += 1;
            }
            b'\n' if !long => return j, // unterminated short literal
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Best-effort `file://` base IRI for a Turtle document. Returns `None` when the
/// path cannot be canonicalised or rendered as UTF-8. Every path byte outside
/// the RFC 3986 unreserved set (and the `/` separator) is percent-encoded, so a
/// path containing IRI-reserved characters (`#`, `?`, `%`, space, …) produces a
/// correct base rather than one where, e.g., a literal `#` is misread as a
/// fragment delimiter.
fn file_base_iri(path: &Path) -> Option<String> {
    let abs = std::fs::canonicalize(path).ok()?;
    let s = abs.to_str()?;
    let mut out = String::from("file://");
    for &b in s.as_bytes() {
        match b {
            b'/' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(
                    char::from_digit((b >> 4) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((b & 0xf) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    Some(out)
}
