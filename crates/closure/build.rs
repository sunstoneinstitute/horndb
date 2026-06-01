#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::env;
#[cfg(feature = "vendored")]
use std::path::Path;
#[cfg(any(feature = "vendored", feature = "regen-bindings"))]
use std::path::PathBuf;
#[cfg(feature = "vendored")]
use std::process::Command;

/// Directory of the vendored SuiteSparse:GraphBLAS submodule headers.
#[cfg(feature = "regen-bindings")]
const VENDOR_INCLUDE: &str = "vendor/GraphBLAS/Include";

/// Path to the submodule's cmake version file (relative to this crate).
#[cfg(feature = "vendored")]
const VERSION_CMAKE: &str = "vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake";

/// Pure, unit-tested helpers (see `tests/build_shared.rs`).
#[cfg(feature = "vendored")]
#[path = "build/shared.rs"]
mod shared;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    // 1. Build (or reuse) the vendored submodule. Sets PKG_CONFIG_PATH so the
    //    shared probe below finds the freshly built (or cached) lib.
    #[cfg(feature = "vendored")]
    build_vendored();

    // 2. Probe GraphBLAS (vendored install or system) and emit the link flags.
    let lib = probe_graphblas();

    // 3. Regenerate bindings only when explicitly asked.
    #[cfg(feature = "regen-bindings")]
    generate_bindings(&lib);

    let _ = &lib;
}

/// Compile the vendored GraphBLAS once per `(target, version)` into a shared
/// dir anchored to the main worktree, reusing it across worktrees. Concurrent
/// builders are serialised with an advisory flock (`fs4`); the rest wait up to
/// 30 minutes. The builder writes its pid into the lock file for diagnostics.
#[cfg(feature = "vendored")]
fn build_vendored() {
    use std::fs;
    use std::time::{Duration, Instant};

    // A version bump (submodule move) must retrigger the build.
    println!("cargo:rerun-if-changed={VERSION_CMAKE}");

    let ver = read_graphblas_version();
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let shared = shared_build_dir(&target, &ver);
    let install = shared.join("install");
    let marker = shared.join(".complete");
    let lock_path = shared.join(".build.lock");

    if !marker.exists() {
        fs::create_dir_all(&shared)
            .unwrap_or_else(|e| panic!("creating {}: {e}", shared.display()));
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap_or_else(|e| panic!("opening {}: {e}", lock_path.display()));

        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        // Diagnostics are emitted once, not every poll, to avoid flooding the
        // build log with one `cargo:warning` per 2s of waiting.
        let mut announced_wait = false;
        let mut announced_gone = false;
        loop {
            // Fully-qualified trait calls: on Rust 1.88 `try_lock`/`unlock`
            // collide with the soon-to-be-stabilised inherent `std::fs::File`
            // methods, which `-D warnings` rejects. FQ syntax pins fs4's trait
            // method and stays correct after std stabilises its own.
            let acquired = match fs4::FileExt::try_lock(&lock_file) {
                Ok(()) => true,
                Err(fs4::TryLockError::WouldBlock) => false,
                Err(e) => panic!("locking {}: {e}", lock_path.display()),
            };
            let timed_out = Instant::now() >= deadline;

            match shared::decide(marker.exists(), acquired, timed_out) {
                shared::LockStep::UseInstall => {
                    if acquired {
                        let _ = fs4::FileExt::unlock(&lock_file);
                    }
                    break;
                }
                shared::LockStep::Build => {
                    // Sticky note for any waiter: who is building.
                    let _ = fs::write(&lock_path, format!("{}\n", std::process::id()));
                    cmake_build_graphblas(&install);
                    fs::write(&marker, b"ok\n")
                        .unwrap_or_else(|e| panic!("writing {}: {e}", marker.display()));
                    let _ = fs4::FileExt::unlock(&lock_file);
                    break;
                }
                shared::LockStep::Wait => {
                    let holder = read_pid(&lock_path);
                    if !announced_wait {
                        println!(
                            "cargo:warning=horndb-closure: waiting for GraphBLAS {ver} build \
                             (holder pid {holder:?}); this is normal across parallel worktrees"
                        );
                        announced_wait = true;
                    }
                    if !announced_gone {
                        if let Some(pid) = holder {
                            if !pid_is_alive(pid) {
                                println!(
                                    "cargo:warning=horndb-closure: GraphBLAS builder pid {pid} \
                                     appears gone; will retry once its flock is released"
                                );
                                announced_gone = true;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
                shared::LockStep::Fail => panic!(
                    "GraphBLAS {ver} build still locked after 30 min (lock: {}, holder pid: {:?}); \
                     remove the lock file if the build is wedged",
                    lock_path.display(),
                    read_pid(&lock_path),
                ),
            }
        }
    }

    // Both the build and reuse paths must point the probe at the shared install
    // and emit the OpenMP link-search (linking happens regardless of who built).
    prepend_pkg_config_path(&install);
    emit_openmp_link_search();
}

/// Compute `<root>/crates/closure/vendor/.shared-build/<target>/<ver>` where
/// `<root>` is the main worktree. Falls back to a crate-local dir when git is
/// unavailable (e.g. building from a source tarball) — correctness is preserved,
/// only cross-worktree sharing is lost.
#[cfg(feature = "vendored")]
fn shared_build_dir(target: &str, ver: &str) -> PathBuf {
    let base = main_worktree_root()
        .map(|root| root.join("crates/closure/vendor/.shared-build"))
        .unwrap_or_else(|| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
                .join("vendor/.shared-build")
        });
    base.join(target).join(ver)
}

/// The main worktree root = parent of `git rev-parse --git-common-dir`
/// (which yields `<root>/.git`). `None` if git is unavailable or errors.
#[cfg(feature = "vendored")]
fn main_worktree_root() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    common.parent().map(Path::to_path_buf)
}

/// Read and parse the pinned GraphBLAS version from the submodule's cmake file.
#[cfg(feature = "vendored")]
fn read_graphblas_version() -> String {
    let contents = std::fs::read_to_string(VERSION_CMAKE)
        .unwrap_or_else(|e| panic!("reading {VERSION_CMAKE}: {e}"));
    shared::parse_version(&contents)
        .unwrap_or_else(|| panic!("could not parse GraphBLAS version from {VERSION_CMAKE}"))
}

/// Best-effort read of the pid the build holder wrote into the lock file.
#[cfg(feature = "vendored")]
fn read_pid(lock_path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(lock_path).ok()?;
    shared::parse_pid(&contents)
}

/// `kill -0 <pid>`: true if the process exists. Diagnostic only — flock, not
/// this check, is what makes a dead builder's lock reclaimable.
#[cfg(feature = "vendored")]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run the cmake build of `vendor/GraphBLAS`, installing into `install`.
#[cfg(feature = "vendored")]
fn cmake_build_graphblas(install: &Path) {
    let mut cfg = cmake::Config::new("vendor/GraphBLAS");
    cfg.out_dir(install)
        .profile("Release")
        .define("BUILD_TESTING", "OFF")
        // SuiteSparse defaults BUILD_SHARED_LIBS=ON. The GraphBLAS-specific
        // GRAPHBLAS_BUILD_STATIC_LIBS flag only *disables* the static build
        // when shared is on; it never forces static ON, so on its own it yields
        // only a `.dylib`, no `.a`, and an empty `Libs.private`. Turn shared off
        // and static on explicitly: this produces a real static archive AND
        // populates GraphBLAS.pc's Libs.private with the transitive deps (libm,
        // OpenMP) that `statik(true)` needs for a self-contained static link.
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_STATIC_LIBS", "ON")
        .define("GRAPHBLAS_USE_JIT", "OFF");

    #[cfg(feature = "openmp")]
    {
        let omp_prefix = openmp_prefix();
        cfg.define("OpenMP_ROOT", &omp_prefix);
        if cfg!(target_os = "macos") {
            // Help CMake's find_package(OpenMP) locate Homebrew libomp.
            cfg.define("CMAKE_PREFIX_PATH", &omp_prefix);
        }
    }

    #[cfg(not(feature = "openmp"))]
    {
        cfg.define("CMAKE_DISABLE_FIND_PACKAGE_OpenMP", "ON");
    }

    cfg.build();
}

/// Prepend the install's pkgconfig dir(s) to PKG_CONFIG_PATH so the probe
/// resolves the vendored library and its static `Libs.private` deps.
#[cfg(feature = "vendored")]
fn prepend_pkg_config_path(install: &Path) {
    let mut pc_dirs: Vec<PathBuf> = vec![
        install.join("lib").join("pkgconfig"),
        install.join("lib64").join("pkgconfig"),
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

/// On macOS + OpenMP, the generated GraphBLAS.pc carries `-lomp` in
/// Libs.private but no `-L` for it (Homebrew libomp lives outside the default
/// search path), so the static link needs the directory. Emitted on BOTH the
/// build and reuse paths because linking happens regardless of who compiled.
#[cfg(feature = "vendored")]
fn emit_openmp_link_search() {
    #[cfg(feature = "openmp")]
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-search=native={}/lib", openmp_prefix());
    }
}

/// Discover the libomp prefix: LIBOMP_PREFIX, else HOMEBREW_PREFIX/opt/libomp,
/// else the Apple Silicon Homebrew default.
#[cfg(all(feature = "vendored", feature = "openmp"))]
fn openmp_prefix() -> String {
    env::var("LIBOMP_PREFIX")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            env::var("HOMEBREW_PREFIX")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|p| format!("{p}/opt/libomp"))
        })
        .unwrap_or_else(|| "/opt/homebrew/opt/libomp".to_string())
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
        .allowlist_function("GrB_.*")
        .allowlist_function("GxB_.*")
        .allowlist_type("GrB_.*")
        .allowlist_type("GxB_.*")
        .allowlist_var("GrB_.*")
        .allowlist_var("GxB_.*")
        .generate_comments(false)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

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
