#![cfg(all(feature = "server", feature = "reasoner"))]
//! HDB-125: `serve --materialize` must not silently answer queries from an
//! OWL 2 RL *inconsistent* closure. One TBox — `A owl:disjointWith B` with an
//! individual typed as both — drives all three `[reasoning].on_inconsistency`
//! policies through the real compiled `serve` binary.
//!
//! Ports are fixed (not `:0`) and disjoint per test, matching
//! `serve_config_wiring.rs`, so the tests run concurrently under nextest.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;

/// Kills and reaps the child on drop so a failed assertion never leaks a
/// server process holding a test port.
struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `A owl:disjointWith B` plus `x a A, x a B` — rule `cax-dw` infers
/// `x a owl:Nothing`, the OWL 2 RL inconsistency marker.
const DISJOINT_CLASH: &str = concat!(
    "<http://ex/A> <http://www.w3.org/2002/07/owl#disjointWith> <http://ex/B> .\n",
    "<http://ex/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/A> .\n",
    "<http://ex/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/B> .\n",
);

/// Write the clashing data plus a `config.toml` binding `port` with the given
/// policy. Returns `(data_path, config_path)`.
fn fixture(dir: &Path, port: u16, policy: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let data = dir.join("clash.nt");
    std::fs::write(&data, DISJOINT_CLASH).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "[server]\nbind = \"127.0.0.1:{port}\"\n[reasoning]\non_inconsistency = \"{policy}\"\n"
        ),
    )
    .unwrap();
    (data, cfg)
}

fn serve_cmd(data: &Path, cfg: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.args([
        "--data",
        data.to_str().unwrap(),
        "--config",
        cfg.to_str().unwrap(),
        "--materialize",
    ])
    .env_remove("HORNDB_CONFIG")
    .env_remove("HORNDB_SERVER__BIND");
    cmd
}

/// Poll `/readyz` until it answers 200. A successful *connect* is not enough:
/// the socket binds before the startup load runs (HDB-124), so the
/// materialization — and with it the inconsistency policy — may not have
/// happened yet.
fn wait_for_ready(addr: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(resp) = get_raw(addr, "/readyz") {
            if resp.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(Instant::now() < deadline, "{addr} never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Minimal HTTP/1.1 GET (no `reqwest` dev-dependency, matching this crate's
/// existing test style). Returns the whole raw response, headers included, so
/// a header assertion needs no extra parsing. `None` if the server is not
/// accepting connections yet.
fn get_raw(addr: &str, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .ok()?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).ok()?;
    Some(String::from_utf8_lossy(&resp).into_owned())
}

fn http_get_raw(addr: &str, path: &str) -> String {
    get_raw(addr, path).expect("GET failed")
}

/// Default policy (`warn`): serve anyway, but publish the gauge and name the
/// `owl:Nothing` individuals on stderr.
#[test]
fn warn_policy_sets_gauge_and_logs_the_witness() {
    let dir = tempdir().unwrap();
    let (data, cfg) = fixture(dir.path(), 18491, "warn");
    let log = dir.path().join("stderr.log");

    let _guard = ServeGuard(
        serve_cmd(&data, &cfg)
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()))
            .spawn()
            .unwrap(),
    );
    wait_for_ready("127.0.0.1:18491", Duration::from_secs(60));

    let resp = http_get_raw("127.0.0.1:18491", "/metrics");
    assert!(
        resp.contains("horndb_reasoning_inconsistent 1"),
        "gauge not published:\n{resp}"
    );
    assert!(
        !resp.to_lowercase().contains("x-horndb-inconsistent"),
        "warn must not stamp the header:\n{resp}"
    );

    let stderr = std::fs::read_to_string(&log).unwrap();
    assert!(
        stderr.contains("owl:Nothing") && stderr.contains("http://ex/x"),
        "startup log must name the inconsistency and its witness:\n{stderr}"
    );
}

/// `reject-startup`: exit non-zero instead of serving. The socket binds
/// before the load runs (HDB-124), so the process tears itself down mid-boot
/// rather than never binding — nothing is left listening either way.
#[test]
fn reject_startup_policy_exits_nonzero() {
    let dir = tempdir().unwrap();
    let (data, cfg) = fixture(dir.path(), 18492, "reject-startup");

    let out = serve_cmd(&data, &cfg).output().unwrap();
    assert!(!out.status.success(), "serve should have failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to serve an inconsistent closure"),
        "exit must explain itself:\n{stderr}"
    );
    assert!(
        TcpStream::connect("127.0.0.1:18492").is_err(),
        "nothing should be listening after a rejected startup"
    );
}

/// `serve-with-flag`: serve, but stamp every response.
#[test]
fn serve_with_flag_policy_stamps_the_response_header() {
    let dir = tempdir().unwrap();
    let (data, cfg) = fixture(dir.path(), 18493, "serve-with-flag");

    let _guard = ServeGuard(
        serve_cmd(&data, &cfg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_ready("127.0.0.1:18493", Duration::from_secs(60));

    let resp = http_get_raw("127.0.0.1:18493", "/metrics");
    assert!(
        resp.to_lowercase().contains("x-horndb-inconsistent: true"),
        "response should carry the inconsistency flag:\n{resp}"
    );
}
