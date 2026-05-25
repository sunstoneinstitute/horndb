//! `show-rule` — print the build-time-generated Rust source for one rule.
//!
//! Usage:
//!
//! ```text
//! cargo run -p horndb-owlrl --bin show-rule -- <rule-id>
//! cargo run -p horndb-owlrl --bin show-rule -- --list
//! cargo run -p horndb-owlrl --bin show-rule -- --all
//! ```
//!
//! The point is that contributors editing `rules.toml` can audit exactly
//! what got compiled without hunting under `target/.../build/horndb-owlrl-*/out/`.
//! See `crates/owlrl/AGENTS.md` for the full codegen story.

use horndb_owlrl::generated::RULES;
use horndb_owlrl::COMPILED_RULES_SOURCE;
use std::process::ExitCode;

const USAGE: &str = "\
show-rule — inspect the compiled output of an OWL 2 RL rule

USAGE:
    cargo run -p horndb-owlrl --bin show-rule -- <rule-id>
    cargo run -p horndb-owlrl --bin show-rule -- --list
    cargo run -p horndb-owlrl --bin show-rule -- --all
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [arg] if arg == "--list" => {
            list_rules();
            ExitCode::SUCCESS
        }
        [arg] if arg == "--all" => {
            print!("{COMPILED_RULES_SOURCE}");
            ExitCode::SUCCESS
        }
        [id] => match show_rule(id) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn list_rules() {
    let max_id = RULES.iter().map(|r| r.id.len()).max().unwrap_or(0);
    println!("{:<width$}  KIND", "ID", width = max_id);
    println!("{}  ----", "-".repeat(max_id));
    for rule in RULES {
        let kind = if rule.delegated {
            "delegated → horndb-closure"
        } else {
            "compiled"
        };
        println!("{:<width$}  {}", rule.id, kind, width = max_id);
    }
    println!("\n{} rule(s) total.", RULES.len());
}

fn show_rule(id: &str) -> Result<(), String> {
    let known: Vec<&str> = RULES.iter().map(|r| r.id).collect();
    if !known.contains(&id) {
        let suggestion = suggest_id(id, &known);
        let hint = if let Some(s) = suggestion {
            format!("\nDid you mean `{s}`?")
        } else {
            String::new()
        };
        return Err(format!(
            "unknown rule id `{id}`. Run with --list to see all {n} rules.{hint}",
            n = RULES.len()
        ));
    }

    let sanitized = id.replace(['-', ':'], "_");
    let needle = format!("pub fn fire_{sanitized}(");
    let Some(fn_start) = COMPILED_RULES_SOURCE.find(&needle) else {
        return Err(format!(
            "internal error: could not locate `fire_{sanitized}` in compiled source"
        ));
    };

    // Walk backwards to include any leading `#[...]` attributes and the
    // `/// Compiled OWL 2 RL rule: <id>` doc comment.
    let preamble_start = walk_back_to_block_start(COMPILED_RULES_SOURCE, fn_start);

    // Walk forwards balancing braces to find the matching `}`.
    let fn_end = find_block_end(COMPILED_RULES_SOURCE, fn_start)
        .ok_or_else(|| "could not find end of fn body".to_string())?;

    print!("{}", &COMPILED_RULES_SOURCE[preamble_start..fn_end]);
    println!();
    Ok(())
}

/// Walk backwards from `pos` skipping over consecutive lines that are
/// attributes (`#[...]`) or doc comments (`///`) so the printed slice
/// includes the rule's preamble.
fn walk_back_to_block_start(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut line_start = line_start_of(bytes, pos);
    loop {
        if line_start == 0 {
            return 0;
        }
        let prev_line_end = line_start - 1; // skip the '\n'
        let prev_line_start = line_start_of(bytes, prev_line_end.saturating_sub(1));
        let prev = &src[prev_line_start..prev_line_end];
        let trimmed = prev.trim_start();
        if trimmed.starts_with("#[") || trimmed.starts_with("///") {
            line_start = prev_line_start;
        } else {
            return line_start;
        }
    }
}

fn line_start_of(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// Given `start` pointing at the beginning of a `pub fn ...(...) -> ... { ... }`
/// declaration, return the index just past the matching closing brace.
fn find_block_end(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    let mut i = start;
    let mut seen_open = false;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                seen_open = true;
            }
            b'}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Trivial Levenshtein-style suggestion: pick the known id that shares the
/// most contiguous prefix with the request.
fn suggest_id<'a>(req: &str, known: &[&'a str]) -> Option<&'a str> {
    known
        .iter()
        .copied()
        .max_by_key(|k| common_prefix_len(req, k))
        .filter(|k| common_prefix_len(req, k) >= 2)
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}
