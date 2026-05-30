#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::env;
#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::path::PathBuf;

/// Directory of the vendored SuiteSparse:GraphBLAS submodule headers.
#[cfg(feature = "regen-bindings")]
const VENDOR_INCLUDE: &str = "vendor/GraphBLAS/Include";

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    // 1. Build the vendored submodule from source if requested. This sets
    //    PKG_CONFIG_PATH so the shared probe below finds the freshly built lib.
    #[cfg(feature = "vendored")]
    build_vendored();

    // 2. Probe GraphBLAS (vendored install or system) and emit the link flags.
    let lib = probe_graphblas();

    // 3. Regenerate bindings only when explicitly asked; otherwise the
    //    checked-in src/bindings.rs is compiled directly by src/ffi.rs.
    #[cfg(feature = "regen-bindings")]
    generate_bindings(&lib);

    // Silence the unused-variable warning when bindings are not regenerated.
    let _ = &lib;
}

/// Build `vendor/GraphBLAS` into `OUT_DIR` as a static library and point
/// PKG_CONFIG_PATH at the resulting pkgconfig dir(s).
#[cfg(feature = "vendored")]
fn build_vendored() {
    let mut cfg = cmake::Config::new("vendor/GraphBLAS");
    cfg.profile("Release")
        .define("BUILD_TESTING", "OFF")
        // SuiteSparse defaults BUILD_SHARED_LIBS=ON. The GraphBLAS-specific
        // GRAPHBLAS_BUILD_STATIC_LIBS flag only *disables* the static build
        // when shared is on (see vendor/GraphBLAS/CMakeLists.txt:49-51); it
        // never forces static ON, so on its own it yields only a `.dylib`, no
        // `.a`, and an empty `Libs.private` in GraphBLAS.pc. Turn the shared
        // build off and the static build on explicitly: this produces a real
        // static archive AND populates GraphBLAS.pc's Libs.private with the
        // transitive deps (libm, OpenMP) that `statik(true)` needs to emit a
        // self-contained static link.
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_STATIC_LIBS", "ON")
        .define("GRAPHBLAS_USE_JIT", "OFF");

    #[cfg(feature = "openmp")]
    {
        // Apple clang's OpenMP detection needs a hint to libomp. Discover the
        // prefix from the environment, falling back to the Homebrew default.
        let omp_prefix = env::var("LIBOMP_PREFIX")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                env::var("HOMEBREW_PREFIX")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|p| format!("{p}/opt/libomp"))
            })
            .unwrap_or_else(|| "/opt/homebrew/opt/libomp".to_string());
        cfg.define("OpenMP_ROOT", &omp_prefix);
        if cfg!(target_os = "macos") {
            // Help CMake's find_package(OpenMP) locate Homebrew libomp.
            cfg.define("CMAKE_PREFIX_PATH", &omp_prefix);
            // The generated GraphBLAS.pc carries `-lomp` in Libs.private but no
            // `-L` for it (it lives outside the default linker search path on
            // Apple Silicon Homebrew), so the static link needs the directory.
            println!("cargo:rustc-link-search=native={omp_prefix}/lib");
        }
    }

    #[cfg(not(feature = "openmp"))]
    {
        cfg.define("CMAKE_DISABLE_FIND_PACKAGE_OpenMP", "ON");
    }

    let dst = cfg.build();

    // Prepend the install's pkgconfig dir(s) to PKG_CONFIG_PATH so the probe
    // below resolves the vendored library and its static Libs.private deps.
    let mut pc_dirs: Vec<PathBuf> = vec![
        dst.join("lib").join("pkgconfig"),
        dst.join("lib64").join("pkgconfig"),
    ];
    if let Some(existing) = env::var_os("PKG_CONFIG_PATH") {
        pc_dirs.extend(env::split_paths(&existing));
    }
    let joined = env::join_paths(pc_dirs).expect("joining PKG_CONFIG_PATH");
    // SAFETY: build scripts are single-threaded; set_var is fine here.
    unsafe {
        env::set_var("PKG_CONFIG_PATH", &joined);
    }
}

/// Probe GraphBLAS via pkg-config and emit `cargo:rustc-link-*` directives.
/// Static linkage is requested only in the vendored case.
fn probe_graphblas() -> pkg_config::Library {
    let mut cfg = pkg_config::Config::new();
    cfg.atleast_version("8.0");

    // The vendored static .pc carries OpenMP/libm in Libs.private; ask
    // pkg-config to resolve those by linking statically.
    #[cfg(feature = "vendored")]
    cfg.statik(true);

    match cfg.probe("GraphBLAS") {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!(
                "\n\
                =====================================================\n\
                horndb-closure: SuiteSparse:GraphBLAS not found.\n\
                pkg-config error: {e}\n\n\
                Either build the bundled copy (default features build the\n\
                vendored submodule) or install GraphBLAS and re-run with\n\
                --no-default-features:\n  \
                  macOS:         brew install suite-sparse pkg-config\n  \
                  Debian/Ubuntu: sudo apt-get install libsuitesparse-dev pkg-config\n\n\
                On Apple Silicon you may also need:\n  \
                  export PKG_CONFIG_PATH=/opt/homebrew/opt/suite-sparse/lib/pkgconfig:$PKG_CONFIG_PATH\n\
                =====================================================\n"
            );
            std::process::exit(1);
        }
    }
}

/// Regenerate the FFI bindings from `wrapper.h` against the GraphBLAS headers.
#[cfg(feature = "regen-bindings")]
fn generate_bindings(lib: &pkg_config::Library) {
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        // Allowlist the GraphBLAS surface so we do not pull in half of libc.
        .allowlist_function("GrB_.*")
        .allowlist_function("GxB_.*")
        .allowlist_type("GrB_.*")
        .allowlist_type("GxB_.*")
        .allowlist_var("GrB_.*")
        .allowlist_var("GxB_.*")
        .generate_comments(false)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // Search the probe's include paths plus the vendored header dir.
    for path in &lib.include_paths {
        bindings_builder = bindings_builder.clang_arg(format!("-I{}", path.display()));
    }
    bindings_builder = bindings_builder.clang_arg(format!("-I{VENDOR_INCLUDE}"));

    let bindings = bindings_builder
        .generate()
        .expect("Unable to generate GraphBLAS bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
