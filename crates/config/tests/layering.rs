//! End-to-end layering tests over real temp directories.

use std::fs;

use horndb_config::{load, ByteSize, CliOverrides, LoadInputs};
use tempfile::tempdir;

/// Build a `LoadInputs` pointing at a base file, with an isolated config.d.
fn inputs_for(base: &std::path::Path) -> LoadInputs {
    LoadInputs {
        cli_config_path: Some(base.to_path_buf()),
        ..Default::default()
    }
}

#[test]
fn config_d_fragment_overrides_base_and_orders_lexically() {
    let dir = tempdir().unwrap();
    let cfg_d = dir.path().join("config.d");
    fs::create_dir(&cfg_d).unwrap();

    let base = dir.path().join("config.toml");
    fs::write(
        &base,
        format!(
            "[server]\nbind = \"1.1.1.1:1\"\nconfig_dirs = [\"{}\"]\n[server.limits]\nmax_result_rows = 1\n",
            cfg_d.display()
        ),
    )
    .unwrap();
    fs::write(
        &cfg_d.join("00-a.toml"),
        "[server.limits]\nmax_result_rows = 2\n",
    )
    .unwrap();
    fs::write(
        &cfg_d.join("99-z.toml"),
        "[server.limits]\nmax_result_rows = 3\n",
    )
    .unwrap();

    let cfg = load(&inputs_for(&base)).unwrap();
    // 99-z wins over 00-a wins over base.
    assert_eq!(cfg.server.limits.max_result_rows, 3);
    // A key only in the base survives the merge.
    assert_eq!(cfg.server.bind, "1.1.1.1:1");
}

#[test]
fn multiple_config_dirs_pool_and_sort_by_filename() {
    let dir = tempdir().unwrap();
    // Two independent drop-in directories: a "manual" one and an "operator" one.
    let manual = dir.path().join("manual.d");
    let operator = dir.path().join("operator.d");
    fs::create_dir(&manual).unwrap();
    fs::create_dir(&operator).unwrap();

    let base = dir.path().join("config.toml");
    fs::write(
        &base,
        format!(
            "[server]\nconfig_dirs = [\"{}\", \"{}\"]\n[server.limits]\nmax_result_rows = 1\n",
            manual.display(),
            operator.display()
        ),
    )
    .unwrap();
    // Operator drops 50-*, manual overrides with 90-*: cross-directory filename
    // order means 90-* is applied last and wins, regardless of directory.
    fs::write(
        &operator.join("50-op.toml"),
        "[server.limits]\nmax_result_rows = 2\n",
    )
    .unwrap();
    fs::write(
        &manual.join("90-override.toml"),
        "[server.limits]\nmax_result_rows = 3\n",
    )
    .unwrap();
    let cfg = load(&inputs_for(&base)).unwrap();
    assert_eq!(cfg.server.limits.max_result_rows, 3);

    // Same filename in both dirs: the later directory (operator) wins the tie.
    fs::write(
        &manual.join("50-op.toml"),
        "[server.limits]\nmax_result_rows = 7\n",
    )
    .unwrap();
    fs::remove_file(manual.join("90-override.toml")).unwrap();
    let cfg = load(&inputs_for(&base)).unwrap();
    assert_eq!(cfg.server.limits.max_result_rows, 2); // operator.d/50-op wins the tie
}

#[test]
fn cli_override_beats_file() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server]\nbind = \"1.1.1.1:1\"\n").unwrap();

    let mut inputs = inputs_for(&base);
    inputs.cli_overrides = CliOverrides {
        bind: Some("2.2.2.2:2".into()),
        ..Default::default()
    };
    let cfg = load(&inputs).unwrap();
    assert_eq!(cfg.server.bind, "2.2.2.2:2"); // CLI wins over the file
}

#[test]
fn missing_default_file_is_ok_missing_explicit_is_error() {
    // Explicit path that does not exist -> error.
    let inputs = LoadInputs {
        cli_config_path: Some("/nonexistent/nope.toml".into()),
        ..Default::default()
    };
    assert!(load(&inputs).is_err());

    // No file anywhere (default path unlikely to exist in CI) -> defaults.
    let cfg = load(&LoadInputs::default()).unwrap();
    assert_eq!(cfg.server.limits.max_query_memory, None::<ByteSize>);
}

#[test]
fn unknown_key_in_base_file_errors_with_key_name() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server]\nbnid = \"oops\"\n").unwrap();

    let err = load(&inputs_for(&base)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("bnid"), "error should name the bad key: {msg}");
    assert!(
        msg.contains("config.toml"),
        "error should name the source file: {msg}"
    );
}

#[test]
fn bad_duration_value_errors() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server.limits]\nquery_timeout = \"30x\"\n").unwrap();

    let err = load(&inputs_for(&base)).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("duration"), "{err}");
}
