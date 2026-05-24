use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    let lib = match pkg_config::Config::new()
        .atleast_version("8.0")
        .probe("GraphBLAS")
    {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!(
                "\n\
                =====================================================\n\
                horndb-closure: SuiteSparse:GraphBLAS not found.\n\
                pkg-config error: {e}\n\n\
                Install instructions:\n  \
                  macOS:       brew install suite-sparse pkg-config\n  \
                  Debian/Ubuntu: sudo apt-get install libsuitesparse-dev pkg-config\n\n\
                On Apple Silicon you may also need:\n  \
                  export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH\n\
                =====================================================\n"
            );
            std::process::exit(1);
        }
    };

    // pkg_config already emits `cargo:rustc-link-lib=...` and `cargo:rustc-link-search=...`.
    // We still tell cargo about the include paths for bindgen.
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        // Allowlist GraphBLAS surface so we do not pull half of libc.
        .allowlist_function("GrB_.*")
        .allowlist_function("GxB_.*")
        .allowlist_type("GrB_.*")
        .allowlist_type("GxB_.*")
        .allowlist_var("GrB_.*")
        .allowlist_var("GxB_.*")
        // GraphBLAS uses a lot of macro-like extern constants we want as raw items.
        .generate_comments(false)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &lib.include_paths {
        bindings_builder = bindings_builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = bindings_builder
        .generate()
        .expect("Unable to generate GraphBLAS bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
