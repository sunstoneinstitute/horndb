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

use std::io::{Read, Write};
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

/// Minimal hand-rolled HTTP/1.1 client (no `reqwest` dev-dependency, matching
/// this crate's existing "avoid the extra dep in tests" style, e.g. the
/// percent-decoder in `server/query.rs`): POST a raw
/// `application/sparql-query` body and return `(status, body)`.
/// `Connection: close` makes end-of-response detectable by EOF.
fn http_post_sparql_query(addr: &str, path: &str, query: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let body = query.as_bytes();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/sparql-query\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).unwrap();
    let resp = String::from_utf8_lossy(&resp).into_owned();
    let status = resp
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = resp
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// HDB-112: `serve --data` did not collect `.nq`/`.trig` files at all (only
/// `.nt`/`.ttl`), so a dataset-format catalog — one named graph per
/// dataset — could not be loaded at server start. Proves the startup path
/// now routes an `.nq` file's quads to their own named graphs: a two-graph
/// `.nq` file loads, and a `GRAPH <g> { ... }` query against each graph
/// returns only that graph's triples.
#[test]
fn nquads_data_file_loads_named_graphs() {
    let dir = tempdir().unwrap();
    let data = dir.path().join("d.nq");
    std::fs::write(
        &data,
        "<http://ex/a> <http://ex/p> <http://ex/b> <http://ex/g1> .\n\
         <http://ex/c> <http://ex/p> <http://ex/d> <http://ex/g2> .\n",
    )
    .unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbind = \"127.0.0.1:18479\"\n").unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
        &[],
    );
    wait_for_connect("127.0.0.1:18479", Duration::from_secs(10));

    let (status1, body1) = http_post_sparql_query(
        "127.0.0.1:18479",
        "/query",
        "SELECT ?s ?o WHERE { GRAPH <http://ex/g1> { ?s <http://ex/p> ?o } }",
    );
    assert_eq!(status1, 200, "body: {body1}");
    assert!(body1.contains("http://ex/a"), "body: {body1}");
    assert!(body1.contains("http://ex/b"), "body: {body1}");
    assert!(
        !body1.contains("http://ex/c") && !body1.contains("http://ex/d"),
        "GRAPH <g1> must not see g2's triple: {body1}"
    );

    let (status2, body2) = http_post_sparql_query(
        "127.0.0.1:18479",
        "/query",
        "SELECT ?s ?o WHERE { GRAPH <http://ex/g2> { ?s <http://ex/p> ?o } }",
    );
    assert_eq!(status2, 200, "body: {body2}");
    assert!(body2.contains("http://ex/c"), "body: {body2}");
    assert!(body2.contains("http://ex/d"), "body: {body2}");
    assert!(
        !body2.contains("http://ex/a") && !body2.contains("http://ex/b"),
        "GRAPH <g2> must not see g1's triple: {body2}"
    );
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

/// PLAN-28-03 Task 2: `AppState.cfg` must reflect the loaded
/// `[server.limits]`. `rdf12` is the observable half of `SparqlConfig` at
/// this point in SPEC-28 phase 3 (`default_graph` is threaded but not yet
/// consumed by the executor — PLAN-28-03 Task 3): with `rdf12 = true` in the
/// config file, an RDF 1.2 triple-term pattern over `/query` must be
/// accepted rather than rejected.
#[test]
fn rdf12_config_flows_to_appstate_cfg() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[server]\nbind = \"127.0.0.1:18477\"\n[server.limits]\nrdf12 = true\n",
    )
    .unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
        &[],
    );
    wait_for_connect("127.0.0.1:18477", Duration::from_secs(10));

    let query =
        "SELECT ?s WHERE { ?s <http://ex/claims> <<( <http://ex/Bob> <http://ex/age> 30 )>> }";
    let (status, body) = http_post_sparql_query("127.0.0.1:18477", "/query", query);
    assert_eq!(
        status, 200,
        "rdf12 = true in config must let a triple-term pattern through: {body}"
    );
}

/// The flip side of `rdf12_config_flows_to_appstate_cfg`: with no
/// `[server.limits].rdf12` set (default `false`), the same triple-term
/// query must still 400 — proving the earlier test's 200 came from the
/// config value actually reaching `AppState.cfg`, not from the handler
/// ignoring `rdf12` altogether.
#[test]
fn rdf12_defaults_to_off_rejecting_triple_term_patterns() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server]\nbind = \"127.0.0.1:18478\"\n").unwrap();

    let _guard = spawn_serve(
        &[
            "--data",
            data.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
        &[],
    );
    wait_for_connect("127.0.0.1:18478", Duration::from_secs(10));

    let query =
        "SELECT ?s WHERE { ?s <http://ex/claims> <<( <http://ex/Bob> <http://ex/age> 30 )>> }";
    let (status, body) = http_post_sparql_query("127.0.0.1:18478", "/query", query);
    assert_eq!(
        status, 400,
        "default AppState.cfg must reject triple-term patterns: {body}"
    );
    assert!(body.contains("triple-term"), "body: {body}");
}

/// SPEC-28 S3/D2: an unrecognized `[server.limits].default_graph` value is
/// startup-fatal. `default_graph` is a serde-level enum
/// (`horndb_config::DefaultGraph`), so the rejection comes from
/// `horndb_config::load()` itself and carries the same source (file + key)
/// attribution as its siblings below (`unknown_config_key_...`,
/// `out_of_range_value_...`) — SPEC-26 S1's requirement, gotten for free
/// instead of by hand (contrast `[simd].max_isa`, a free string checked in
/// `serve.rs`, which names the value but not the file).
#[test]
fn invalid_default_graph_exits_nonzero_naming_the_source() {
    let dir = tempdir().unwrap();
    let data = write_data_file(dir.path());
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[server.limits]\ndefault_graph = \"bogus\"\n").unwrap();

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
    assert!(
        stderr.contains("default_graph"),
        "stderr should name the bad key: {stderr}"
    );
    assert!(
        stderr.contains("bogus"),
        "stderr should name the bad value: {stderr}"
    );
}
