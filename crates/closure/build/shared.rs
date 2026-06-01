//! Pure helpers for `build.rs`, isolated here so they can be unit-tested
//! (`tests/build_shared.rs`) without cmake, git, flock, or any IO. Keep this
//! file dependency-free: std only.

/// Parse the GraphBLAS version (e.g. `"10.3.0"`) from the contents of
/// `vendor/GraphBLAS/cmake_modules/GraphBLAS_version.cmake`.
///
/// Looks for the three `set ( GraphBLAS_VERSION_{MAJOR,MINOR,SUB} <n> ... )`
/// lines and reads the first integer token after each field name. Lines that
/// reference the field via `${...}` interpolation (e.g. the `message(...)`
/// line) yield a non-numeric next token and are skipped.
pub fn parse_version(cmake: &str) -> Option<String> {
    let major = extract_field(cmake, "GraphBLAS_VERSION_MAJOR")?;
    let minor = extract_field(cmake, "GraphBLAS_VERSION_MINOR")?;
    let sub = extract_field(cmake, "GraphBLAS_VERSION_SUB")?;
    Some(format!("{major}.{minor}.{sub}"))
}

fn extract_field(cmake: &str, name: &str) -> Option<u64> {
    cmake.lines().find_map(|line| {
        let (_, rest) = line.split_once(name)?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// One iteration's decision in the build-lock loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStep {
    /// We hold the lock and there is no completed build — compile now.
    Build,
    /// A completed build exists — use it (release the lock if we hold one).
    UseInstall,
    /// Someone else is building and the deadline has not passed — sleep & retry.
    Wait,
    /// Deadline exceeded while another process still holds the lock — give up.
    Fail,
}

/// Decide what to do this iteration.
///
/// Correctness rests on the flock, not on liveness checks: if the builder dies,
/// the kernel releases its flock and a later iteration acquires it. So holder
/// liveness is *not* an input here — it is logged by the caller for diagnostics
/// only. `timed_out` is consulted solely when we are still waiting.
pub fn decide(marker_exists: bool, lock_acquired: bool, timed_out: bool) -> LockStep {
    if lock_acquired {
        if marker_exists {
            LockStep::UseInstall
        } else {
            LockStep::Build
        }
    } else if marker_exists {
        LockStep::UseInstall
    } else if timed_out {
        LockStep::Fail
    } else {
        LockStep::Wait
    }
}
