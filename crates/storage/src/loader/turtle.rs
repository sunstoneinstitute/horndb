//! Streaming Turtle bulk loader.
//!
//! Uses `oxttl::TurtleParser` to stream triples from any `Read` source,
//! batching into the dictionary + tier in chunks of [`BATCH_SIZE`]. Turtle
//! carries no graph component, so every triple lands in the default graph
//! (SPEC-02 F7's reserved sentinel). Prefixes, `a`, collections, and blank-node
//! property lists are expanded by the parser before they reach the dictionary.
//!
//! Relative IRIs resolve against a base IRI. [`load_turtle_file`] derives a
//! best-effort `file://` base from the document path (the conventional RDF
//! base), so files that use document-relative IRIs load. [`load_turtle_reader`]
//! has no inherent base and parses base-less (relative IRIs error);
//! [`load_turtle_reader_with_base`] lets a caller supply one explicitly.

use crate::error::{Result, StorageError};
use crate::loader::{subject_to_term, LoadStats, BATCH_SIZE};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::Term;
use oxttl::TurtleParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

pub fn load_turtle_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    // Best-effort document base so relative IRIs resolve against the file's own
    // location. Drop it if it does not form a valid base IRI (rather than
    // failing the import), leaving base-less parsing for that pathological path.
    let base = file_base_iri(path).filter(|b| TurtleParser::new().with_base_iri(b).is_ok());
    let mut stats = load_turtle_reader_with_base(store, reader, base.as_deref())?;
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
    let start = Instant::now();
    let mut parser = TurtleParser::new();
    if let Some(base) = base_iri {
        parser = parser
            .with_base_iri(base)
            .map_err(|e| StorageError::TurtleParse(format!("invalid base IRI {base:?}: {e}")))?;
    }
    let iter = parser.for_reader(reader);

    let mut batch: Vec<(crate::term::GraphId, _, _, _)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    for t in iter {
        let triple = t.map_err(|e| StorageError::TurtleParse(format!("{e}")))?;
        let s_term = subject_to_term(triple.subject);
        let p_term = Term::NamedNode(triple.predicate);
        let o_term = triple.object;

        let s_id = store.dictionary().intern(&s_term)?;
        let p_id = store.dictionary().intern(&p_term)?;
        let o_id = store.dictionary().intern(&o_term)?;
        batch.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        total += 1;
        if batch.len() >= BATCH_SIZE {
            store.tier().insert_quad_batch(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        store.tier().insert_quad_batch(&batch)?;
    }

    Ok(LoadStats {
        triples: total,
        bytes_read: 0, // file caller will overwrite
        elapsed_ms: start.elapsed().as_millis() as u64,
        dictionary_size: store.dictionary().len() as u64,
    })
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
