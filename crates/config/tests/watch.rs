//! SPEC-26 S3 live watch and reload, driven against real files on disk.
//!
//! Every test edits a real `config.toml` (or a `config.d` fragment) under a
//! `tempfile` directory and waits for the watcher to publish, rather than
//! calling the reload cycle directly — a direct call would prove nothing about
//! whether the watcher is still armed.
//!
//! Reads of the process-global metrics registry assume one test per process,
//! which is what `cargo nextest` gives (the workspace's mandated runner).

use std::path::Path;
use std::time::{Duration, Instant};

use horndb_config::{load, ConfigHandle, LoadInputs};
use horndb_metrics::labels::{ReloadResult, ReloadResultLabel};
use tempfile::TempDir;

/// Poll `cond` until it holds or five seconds pass. Bounded waiting rather
/// than a fixed sleep: the filesystem-event round trip is fast but not
/// deterministic, so a fixed sleep is either flaky or slow.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn reload_count(result: ReloadResult) -> u64 {
    horndb_metrics::metrics()
        .config
        .reloads
        .get_or_create(&ReloadResultLabel { result })
        .get()
}

/// A config file with a short debounce, so the tests settle quickly.
fn write_config(path: &Path, body: &str) {
    std::fs::write(path, format!("[reload]\ndebounce = \"20ms\"\n{body}")).unwrap();
}

/// Load the base file and start the watcher over it. Returns the handle and the
/// watcher guard (dropping the guard stops the watcher).
fn start(dir: &TempDir, body: &str) -> (ConfigHandle, horndb_config::ConfigWatcher) {
    let base = dir.path().join("config.toml");
    write_config(&base, body);
    let inputs = LoadInputs {
        cli_config_path: Some(base),
        ..Default::default()
    };
    let handle = ConfigHandle::new(load(&inputs).unwrap());
    let watcher = horndb_config::watch(inputs, handle.clone()).unwrap();
    (handle, watcher)
}

/// AC1: editing a hot key takes effect within the debounce window and bumps the
/// generation and the `applied` counter.
#[test]
fn hot_key_edit_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _w) = start(&dir, "[server.limits]\nmax_result_rows = 10\n");
    assert_eq!(handle.current().server.limits.max_result_rows, 10);
    assert_eq!(handle.generation(), 1);
    let applied_before = reload_count(ReloadResult::Applied);

    write_config(
        &dir.path().join("config.toml"),
        "[server.limits]\nmax_result_rows = 77\n[logging]\nlevel = \"debug\"\n",
    );

    wait_for("the hot-key reload", || handle.generation() == 2);
    let cfg = handle.current();
    assert_eq!(cfg.server.limits.max_result_rows, 77);
    assert_eq!(cfg.logging.level, "debug");
    assert_eq!(
        horndb_metrics::metrics().config.active_generation.get(),
        2,
        "config_active_generation must track the published generation"
    );
    assert_eq!(reload_count(ReloadResult::Applied), applied_before + 1);
    assert!(
        horndb_metrics::metrics().config.last_reload_unixtime.get() > 0,
        "config_last_reload_unixtime must be stamped on an applied reload"
    );
}

/// AC2: an edit that does not validate keeps the previous config live and bumps
/// the `rejected` counter — the generation does not move.
#[test]
fn invalid_edit_keeps_the_previous_config() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _w) = start(&dir, "[server.limits]\nmax_result_rows = 10\n");

    // An unknown key: `deny_unknown_fields` rejects it, naming file and key.
    write_config(
        &dir.path().join("config.toml"),
        "[server.limits]\nmax_reslt_rows = 99\n",
    );
    wait_for("the rejection", || {
        reload_count(ReloadResult::Rejected) >= 1
    });

    assert_eq!(
        handle.current().server.limits.max_result_rows,
        10,
        "a rejected reload must leave the previous config live"
    );
    assert_eq!(
        handle.generation(),
        1,
        "a rejection is not a new generation"
    );
    assert_eq!(reload_count(ReloadResult::Applied), 0);

    // And the watcher is still armed: a subsequent valid edit applies.
    write_config(
        &dir.path().join("config.toml"),
        "[server.limits]\nmax_result_rows = 11\n",
    );
    wait_for("recovery after a rejection", || handle.generation() == 2);
    assert_eq!(handle.current().server.limits.max_result_rows, 11);
}

/// HDB-156: editing in place is not atomic — between the truncate and the
/// write the file is empty, and an empty TOML file parses fine, so every key
/// would silently take its default. A reload that lands in that window must
/// publish nothing: the previous config stays live and nothing counts as
/// `applied`.
#[test]
fn truncation_window_is_not_published_as_defaults() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let (handle, _w) = start(&dir, "[server.limits]\nmax_result_rows = 10\n");

    // Truncate and stall, the way a loaded machine can stall a `>` redirect
    // between opening the file and writing it. 400 ms is many debounce
    // intervals, so the watcher definitely reads the file while it is empty.
    let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        handle.current().server.limits.max_result_rows,
        10,
        "a truncated file must not replace the live config with schema defaults"
    );
    assert_eq!(
        handle.generation(),
        1,
        "a truncated read is not a generation"
    );
    assert_eq!(reload_count(ReloadResult::Applied), 0);

    // Finishing the write applies the edit the operator actually made.
    f.write_all(b"[reload]\ndebounce = \"20ms\"\n[server.limits]\nmax_result_rows = 11\n")
        .unwrap();
    drop(f);
    wait_for("the completed write", || handle.generation() == 2);
    assert_eq!(handle.current().server.limits.max_result_rows, 11);
}

/// AC4: an atomic rename-into-place save (write temp, rename over the target —
/// what most editors and config-management tools do) replaces the file's inode.
/// The watcher must survive it, and must survive a *second* one, which is what
/// proves the watch was not orphaned by the first replacement.
#[test]
fn rename_into_place_save_is_observed_repeatedly() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _w) = start(&dir, "[server.limits]\nmax_result_rows = 1\n");

    for (round, rows) in [(2u64, 2u64), (3, 3)] {
        let tmp = dir.path().join(format!("config.toml.new{round}"));
        write_config(
            &tmp,
            &format!("[server.limits]\nmax_result_rows = {rows}\n"),
        );
        std::fs::rename(&tmp, dir.path().join("config.toml")).unwrap();

        wait_for(&format!("rename-into-place round {round}"), || {
            handle.generation() == round
        });
        assert_eq!(handle.current().server.limits.max_result_rows, rows);
    }
}

/// The watcher covers `config_dirs` too, not just the base file: dropping a new
/// fragment into a watched directory reloads.
#[test]
fn config_d_fragment_triggers_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let dropin = dir.path().join("config.d");
    std::fs::create_dir(&dropin).unwrap();
    let (handle, _w) = start(
        &dir,
        &format!(
            "[server]\nconfig_dirs = [\"{}\"]\n[server.limits]\nmax_result_rows = 5\n",
            dropin.display()
        ),
    );
    assert_eq!(handle.current().server.limits.max_result_rows, 5);

    std::fs::write(
        dropin.join("90-override.toml"),
        "[server.limits]\nmax_result_rows = 500\n",
    )
    .unwrap();

    wait_for("the config.d fragment reload", || handle.generation() == 2);
    assert_eq!(handle.current().server.limits.max_result_rows, 500);
}

/// AC3 (config half): a changed restart-only key is *stored* — a later restart
/// picks it up — and reported as restart-only, so nothing tries to re-bind.
#[test]
fn changed_bind_is_stored_and_flagged_restart_only() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _w) = start(&dir, "[server]\nbind = \"127.0.0.1:18001\"\n");

    write_config(
        &dir.path().join("config.toml"),
        "[server]\nbind = \"127.0.0.1:18002\"\n",
    );
    wait_for("the bind reload", || handle.generation() == 2);

    let new = handle.current();
    assert_eq!(new.server.bind, "127.0.0.1:18002");
    assert_eq!(
        horndb_config::restart_only_changes(
            &{
                let mut old = (*new).clone();
                old.server.bind = "127.0.0.1:18001".to_string();
                old
            },
            &new
        ),
        vec!["[server].bind"]
    );
}
