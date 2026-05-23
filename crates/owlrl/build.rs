//! Build-time rule codegen. Reads rules.toml, emits one `fire_<id>()` per rule
//! into `$OUT_DIR/generated_rules.rs`. Re-runs on changes to any codegen input.

#[path = "codegen/mod.rs"]
mod codegen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let rules_path = manifest_dir.join("rules.toml");
    println!("cargo:rerun-if-changed={}", rules_path.display());
    println!("cargo:rerun-if-changed=codegen/mod.rs");
    println!("cargo:rerun-if-changed=codegen/parse.rs");
    println!("cargo:rerun-if-changed=codegen/emit.rs");
    println!("cargo:rerun-if-changed=codegen/plan.rs");

    let rules = match codegen::parse::parse_file(&rules_path) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("FAILED to parse rules.toml: {e:#}");
            std::process::exit(1);
        }
    };

    let tokens = codegen::emit::emit_all(&rules);
    let syntax_tree: syn::File = match syn::parse2(tokens.clone()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("emitted code is not valid Rust: {e}");
            eprintln!("emitted:\n{tokens}");
            std::process::exit(1);
        }
    };
    let pretty = prettyplease::unparse(&syntax_tree);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("generated_rules.rs");
    fs::write(&out_path, pretty).expect("writing generated_rules.rs");
}
