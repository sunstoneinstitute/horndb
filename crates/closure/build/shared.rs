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
        let idx = line.find(name)?;
        let rest = &line[idx + name.len()..];
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}
