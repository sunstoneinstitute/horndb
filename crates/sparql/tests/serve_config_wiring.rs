#![cfg(feature = "server")]
//! End-to-end coverage for PLAN-26-02 Task 2: the `serve` binary resolves one
//! `ServerConfig` through `horndb-config` and binds the socket from it, with
//! precedence file < `HORNDB_SERVER__BIND` < `--bind`. Each test spawns the
//! real compiled `serve` binary (not just library code) so the coverage is a
//! genuine socket bind, not a unit-level stand-in.
//!
//! Ports are fixed (not `:0`) so the printed/connected address is
//! deterministic proof of *which* layer won; each test in this file uses a
//! disjoint port so they can run concurrently under nextest.

use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use tempfile::tempdir;

/// Kills and reaps the child on drop so a failed assertion never leaks a
/// server process that keeps a test port open for the rest of the run.
struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_data_file(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("d.nt");
    std::fs::write(&p, "<http://ex/a> <http://ex/p> <http://ex/b> .\n").unwrap();
    p
}

/// Poll-connect until `addr` accepts, or panic after `timeout`.
fn wait_for_connect(addr: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("no listener on {addr} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A short grace period is enough to prove `addr` is NOT the bound address:
/// `serve` binds synchronously before printing anything, so if the winning
/// layer bound elsewhere, an immediate connect attempt here reliably refuses.
fn assert_not_listening(addr: &str) {
    assert!(
        TcpStream::connect(addr).is_err(),
        "expected nothing listening on {addr}, but connect succeeded"
    );
}

/// `serve` never exits on success (it serves forever), so `assert_cmd`'s
/// wait-for-completion helpers don't apply here — spawn the compiled binary
/// directly via `std::process::Command` and prove the bind by connecting.
fn spawn_serve(args: &[&str], env: &[(&str, &str)]) -> ServeGuard {
    let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_serve"));
    cmd.args(args)
        .env_remove("HORNDB_CONFIG")
        .env_remove("HORNDB_SERVER__BIND")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    ServeGuard(cmd.spawn().unwrap())
}

#[test]
fn binds_config_file_value_when_no_env_or_flag() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbind = \"127.0.0.1:18471\"\n").unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
        &[],
    );

    wait_for_connect("127.0.0.1:18471", Duration::from_secs(10));
}

#[test]
fn env_var_overrides_config_file_bind() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbind = \"127.0.0.1:18472\"\n").unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
        &[("HORNDB_SERVER__BIND", "127.0.0.1:18473")],
    );

    wait_for_connect("127.0.0.1:18473", Duration::from_secs(10));
    assert_not_listening("127.0.0.1:18472");
}

#[test]
fn cli_flag_overrides_env_and_config_file() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbind = \"127.0.0.1:18474\"\n").unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "--bind",
            "127.0.0.1:18476",
        ],
        &[("HORNDB_SERVER__BIND", "127.0.0.1:18475")],
    );

    wait_for_connect("127.0.0.1:18476", Duration::from_secs(10));
    assert_not_listening("127.0.0.1:18474");
    assert_not_listening("127.0.0.1:18475");
}

#[test]
fn unknown_config_key_exits_nonzero_naming_the_source() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbnid = \"oops\"\n").unwrap();

    let assert = Command::cargo_bin("serve")
        .unwrap()
        .args([
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ])
        .env_remove("HORNDB_CONFIG")
        .env_remove("HORNDB_SERVER__BIND")
        .assert()
        .failure();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bnid"),
        "stderr should name the bad key: {stderr}"
    );
    assert!(
        stderr.contains("config.toml"),
        "stderr should name the source file: {stderr}"
    );
}

#[test]
fn out_of_range_value_exits_nonzero_naming_the_source() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    // `query_timeout` only accepts a HumanDuration string ("30s"); this value
    // fails the units parser (SPEC-26 S2 out-of-range/invalid-value case).
    std::fs::write(
        &cfg,
        "[server.limits]\nquery_timeout = \"not-a-duration\"\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("serve")
        .unwrap()
        .args([
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ])
        .env_remove("HORNDB_CONFIG")
        .env_remove("HORNDB_SERVER__BIND")
        .assert()
        .failure();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("config.toml"),
        "stderr should name the source file: {stderr}"
    );
}

#[test]
fn explicit_config_flag_pointing_at_missing_file_exits_nonzero() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let missing = dir.path().join("does-not-exist.toml");

    let assert = Command::cargo_bin("serve")
        .unwrap()
        .args([
            "--data",
            data.to_str().unwrap(),
            "--config",
            missing.to_str().unwrap(),
        ])
        .env_remove("HORNDB_CONFIG")
        .env_remove("HORNDB_SERVER__BIND")
        .assert()
        .failure();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist.toml"),
        "stderr should name the missing file: {stderr}"
    );
}
