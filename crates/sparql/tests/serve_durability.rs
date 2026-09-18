#![cfg(feature = "server")]
//! HDB-233: `serve` wired to the SPEC-25 S3 write-ahead-log store.
//!
//! What these tests pin, exactly: a write that `POST /update` answered `200`
//! to is on disk when the response is read, so it survives `SIGKILL` — not a
//! clean shutdown, not a checkpoint. That is the store's default
//! `SyncPolicy::EveryBatch`, which fsyncs the log record before the write
//! reaches the tier and therefore before the response is sent. Nothing weaker
//! is claimed and nothing weaker is tested: the process is killed with
//! `SIGKILL` and never given a chance to flush anything.
//!
//! The second test pins the directory lock: a second `serve` on the same
//! directory fails at startup, and stops failing as soon as the first process
//! is killed — a lock that outlived a `SIGKILL` would brick every restart.
//!
//! Each server binds `127.0.0.1:0` and the test reads the real port back off
//! its log line, so a parallel test run cannot collide on a port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use tempfile::tempdir;

const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// A running `serve` and its bound address. Dropping it kills and reaps the
/// child, so a failed assertion never leaks a server holding a store directory
/// for the rest of the run.
struct Serve {
    child: Child,
    addr: String,
    /// Keeps the stderr drain thread alive: the thread stops on a send error,
    /// which closes the pipe's read end and would make the server's next log
    /// line fail. `serve` logs every request, so that happens immediately.
    _log: Receiver<String>,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Serve {
    /// `SIGKILL` and reap. The store gets no chance to checkpoint, sync, or
    /// run any `Drop`, which is the whole point.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

/// Spawn `serve` against `data_dir` on an ephemeral port. Returns the handle
/// once the log line naming the bound address appears, or `Err` with the
/// child's exit status if it died first (the directory-lock case).
fn spawn(data_dir: &Path, extra: &[&str]) -> Result<Serve, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--bind")
        .arg("127.0.0.1:0")
        .args(extra)
        .env_remove("HORNDB_CONFIG")
        .env_remove("HORNDB_SERVER__BIND")
        .env("HORNDB_SERVER__DATA_DIR", data_dir)
        // A short interval so the background checkpoint scheduler actually
        // runs during a test, rather than being dead code here.
        .env("HORNDB_SERVER__CHECKPOINT_INTERVAL", "1s")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Drain stderr on its own thread. `serve` logs every request, so leaving
    // the pipe unread could fill its buffer and block the server.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    match wait_for_addr(&rx, &mut child) {
        Some(addr) => Ok(Serve {
            child,
            addr,
            _log: rx,
        }),
        None => {
            let _ = child.kill();
            let status = child.wait().unwrap();
            Err(format!("serve exited: {status}"))
        }
    }
}

/// Watch the log lines for `listening at http://<addr>`, giving up when the
/// process exits or the deadline passes.
fn wait_for_addr(rx: &Receiver<String>, child: &mut Child) -> Option<String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            if let Some(rest) = line.split("listening at http://").nth(1) {
                return Some(rest.split_whitespace().next().unwrap().to_string());
            }
            continue;
        }
        if child.try_wait().unwrap().is_some() {
            return None;
        }
    }
    None
}

/// Poll `/readyz` until it answers 200. The socket binds before the startup
/// load finishes (HDB-124), so a bare connect does not mean the data is
/// queryable.
fn wait_ready(addr: &str) {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if request(addr, "GET", "/readyz", None).0 == 200 {
            return;
        }
        assert!(Instant::now() < deadline, "{addr} never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Minimal hand-rolled HTTP/1.1 client, matching this crate's existing
/// no-`reqwest`-dev-dependency style (see `serve_config_wiring.rs`).
/// `Connection: close` makes end-of-response detectable by EOF.
fn request(addr: &str, method: &str, path: &str, body: Option<(&str, &str)>) -> (u16, String) {
    let Ok(mut stream) = TcpStream::connect(addr) else {
        return (0, String::new());
    };
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some((content_type, payload)) = body {
        req.push_str(&format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    if let Some((_, payload)) = body {
        stream.write_all(payload.as_bytes()).unwrap();
    }
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).unwrap();
    let resp = String::from_utf8_lossy(&resp).into_owned();
    let status = resp
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, resp)
}

/// `POST /update`, asserting it was accepted. SPARQL Update answers `204 No
/// Content` on success; the response body carries nothing to check, so the
/// status is the whole acknowledgement — and it is what the durability claim
/// hangs on.
fn update(addr: &str, sparql: &str) {
    let (status, body) = request(
        addr,
        "POST",
        "/update",
        Some(("application/sparql-update", sparql)),
    );
    assert!(
        status == 200 || status == 204,
        "update refused with {status}: {body}"
    );
}

fn query(addr: &str, sparql: &str) -> (u16, String) {
    request(
        addr,
        "POST",
        "/query",
        Some(("application/sparql-query", sparql)),
    )
}

const INSERT: &str = concat!(
    "INSERT DATA { <http://ex/durable> <http://ex/p> ",
    "\"survives a kill\" }"
);
const SELECT: &str = "SELECT ?o WHERE { <http://ex/durable> <http://ex/p> ?o }";

#[test]
fn a_write_acknowledged_over_http_survives_sigkill() {
    let dir = tempdir().unwrap();

    let first = spawn(dir.path(), &[]).expect("first serve starts");
    wait_ready(&first.addr);
    update(&first.addr, INSERT);
    let (status, body) = query(&first.addr, SELECT);
    assert_eq!(status, 200);
    assert!(body.contains("survives a kill"), "{body}");

    // No clean shutdown, no checkpoint, no drain: the guarantee under test is
    // that the log record was already fsynced when the 200 above was sent.
    first.kill();

    let second = spawn(dir.path(), &[]).expect("second serve reopens the directory");
    wait_ready(&second.addr);
    let (status, body) = query(&second.addr, SELECT);
    assert_eq!(status, 200);
    assert!(
        body.contains("survives a kill"),
        "the killed process's acknowledged write is gone after restart: {body}"
    );

    // And the restarted process is a working store, not a read-only replay.
    update(
        &second.addr,
        "INSERT DATA { <http://ex/durable> <http://ex/p2> \"after restart\" }",
    );
    let (_, body) = query(
        &second.addr,
        "SELECT ?o WHERE { <http://ex/durable> <http://ex/p2> ?o }",
    );
    assert!(body.contains("after restart"), "{body}");
}

#[test]
fn one_directory_holds_one_serve_at_a_time() {
    let dir = tempdir().unwrap();

    let first = spawn(dir.path(), &[]).expect("first serve starts");
    wait_ready(&first.addr);

    // A second process on the same directory must not start at all — not bind
    // a port, not serve a request.
    let blocked = spawn(dir.path(), &[]);
    assert!(
        blocked.is_err(),
        "a second serve on a held store directory must fail at startup"
    );

    // The lock is the kernel's, tied to an open file descriptor, so a SIGKILL
    // releases it. A lock file that outlived the kill would brick the restart.
    first.kill();
    let third = spawn(dir.path(), &[]).expect("the lock must be released by SIGKILL");
    wait_ready(&third.addr);
    assert_eq!(query(&third.addr, SELECT).0, 200);
}
