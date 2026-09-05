//! Live watch and reload (SPEC-26 S3).
//!
//! [`ConfigHandle`] is the live handle: an `ArcSwap<ServerConfig>` plus a
//! generation counter. Request handlers take a cheap snapshot per request with
//! [`ConfigHandle::current`]; [`watch`] republishes into the same handle when
//! the operator edits a file.
//!
//! The reload cycle is deliberately event-shape-insensitive: any settled
//! filesystem event re-runs the whole [`crate::load`] (re-resolve → re-merge →
//! validate). That makes reload idempotent, so it does not matter whether an
//! editor saved by truncating in place, by renaming a temp file over the
//! target, or by emitting three events for one save. A config that fails to
//! validate is dropped on the floor: the previous config stays live and the
//! rejected-reload counter goes up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use horndb_metrics::labels::{ReloadResult, ReloadResultLabel};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::ConfigError;
use crate::load::{load, resolve_base_path, LoadInputs};
use crate::model::{Limits, ServerConfig};

/// The live `ServerConfig`, published atomically.
///
/// Clone it freely — every clone reads and writes the same cell. A reader pays
/// one atomic load plus an `Arc` clone, so a handler can take a fresh snapshot
/// per request without coordinating with the watcher.
#[derive(Clone)]
pub struct ConfigHandle {
    inner: Arc<Inner>,
}

struct Inner {
    current: ArcSwap<ServerConfig>,
    generation: AtomicU64,
}

impl ConfigHandle {
    /// Publish `cfg` as generation 1 — the startup load.
    pub fn new(cfg: ServerConfig) -> Self {
        let handle = Self {
            inner: Arc::new(Inner {
                current: ArcSwap::from_pointee(cfg),
                generation: AtomicU64::new(1),
            }),
        };
        horndb_metrics::metrics().config.active_generation.set(1);
        handle
    }

    /// A handle over otherwise-default config with `limits` in place. For
    /// callers that assemble a server by hand (tests, library embedders) and
    /// never load a config file.
    pub fn from_limits(limits: Limits) -> Self {
        let mut cfg = ServerConfig::default();
        cfg.server.limits = limits;
        Self::new(cfg)
    }

    /// The config in effect right now. One atomic load; call it per request.
    pub fn current(&self) -> Arc<ServerConfig> {
        self.inner.current.load_full()
    }

    /// Generation of the config [`current`](Self::current) returns: 1 at
    /// startup, +1 per applied reload.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// Swap `cfg` in and return its generation. Publication is atomic: a reader
    /// sees either the whole old config or the whole new one, never a mix.
    fn publish(&self, cfg: ServerConfig) -> u64 {
        self.inner.current.store(Arc::new(cfg));
        self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
}

impl Default for ConfigHandle {
    fn default() -> Self {
        Self::new(ServerConfig::default())
    }
}

/// Keys that a reload stores but cannot apply to the running process. A change
/// to any of these is kept (a later restart honours it) and logged as
/// "requires restart to take effect" — the server never silently claims it went
/// live. Returns the TOML paths of whatever changed, for the log line.
///
/// `[simd]` is here because ISA selection and calibration run once, before the
/// first dispatch; `[server].bind` because the socket is already bound;
/// `[server].config_dirs` because the watch set is established once; the three
/// `[server.limits]` admission keys because the semaphore and the body-limit
/// layer are built at startup; `[reasoning]` because materialization happens at
/// startup. Everything else — the rest of `[server.limits]`, `[logging]`,
/// `[reload]` — is hot.
pub fn restart_only_changes(old: &ServerConfig, new: &ServerConfig) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut check = |changed: bool, key: &'static str| {
        if changed {
            out.push(key);
        }
    };
    check(old.server.bind != new.server.bind, "[server].bind");
    check(
        old.server.config_dirs != new.server.config_dirs,
        "[server].config_dirs",
    );
    check(
        old.server.shutdown_drain != new.server.shutdown_drain,
        "[server].shutdown_drain",
    );
    check(
        old.server.limits.max_concurrent_queries != new.server.limits.max_concurrent_queries,
        "[server.limits].max_concurrent_queries",
    );
    check(
        old.server.limits.queue_timeout != new.server.limits.queue_timeout,
        "[server.limits].queue_timeout",
    );
    check(
        old.server.limits.max_request_body != new.server.limits.max_request_body,
        "[server.limits].max_request_body",
    );
    check(old.simd != new.simd, "[simd]");
    check(old.reasoning != new.reasoning, "[reasoning]");
    out
}

/// A running config watcher. Dropping it stops the watcher and its debounce
/// thread; keep it alive for as long as the server should follow file edits.
pub struct ConfigWatcher {
    // Dropping the notify watcher closes the event channel, which ends the
    // debounce thread's `recv` loop. That is the whole shutdown protocol.
    _watcher: RecommendedWatcher,
}

/// Watch the config sources named by `inputs` and republish into `handle` when
/// they change, debounced by `[reload].debounce`.
///
/// Watches **directories**, not the base file itself: the base file's parent
/// directory plus every `config_dirs` entry, each non-recursively. An editor
/// that saves by renaming a temp file over the target replaces the inode, which
/// would silently orphan a watch on the file — a directory watch keeps seeing
/// the replacement, and every drop-in fragment added or removed later, with no
/// re-arming.
///
/// A watch target that does not exist yet is skipped with a log line rather
/// than failing: `/etc/horndb` is routinely absent on a developer machine, and
/// a missing default config path is not an error (S1).
pub fn watch(inputs: LoadInputs, handle: ConfigHandle) -> Result<ConfigWatcher, ConfigError> {
    let (base_path, _) = resolve_base_path(&inputs);
    let cfg = handle.current();

    let mut targets: BTreeSet<PathBuf> = BTreeSet::new();
    if let Some(parent) = base_path.parent() {
        targets.insert(parent.to_path_buf());
    }
    targets.extend(cfg.server.config_dirs.iter().cloned());

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        // A send failure means the debounce thread is gone (shutdown); there is
        // nothing useful to do about it here.
        let _ = tx.send(res);
    })
    .map_err(|e| watch_error(&base_path, &e))?;

    let mut watched = 0usize;
    for dir in &targets {
        if !dir.is_dir() {
            eprintln!("config: not watching {} (no such directory)", dir.display());
            continue;
        }
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| watch_error(dir, &e))?;
        watched += 1;
    }
    if watched == 0 {
        eprintln!("config: no config directories exist; live reload is inactive");
    }

    let debounce_handle = handle.clone();
    std::thread::Builder::new()
        .name("horndb-config-watch".to_string())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Settle: swallow every further event until the sources have
                // been quiet for one debounce interval. One editor save can be
                // several events, and a `config.d` rewrite can be many.
                let debounce = debounce_handle.current().reload.debounce.0;
                while rx.recv_timeout(debounce).is_ok() {}
                reload_once(&inputs, &debounce_handle);
            }
        })
        .map_err(|e| ConfigError::Watch {
            path: base_path.clone(),
            message: e.to_string(),
        })?;

    Ok(ConfigWatcher { _watcher: watcher })
}

fn watch_error(path: &Path, e: &notify::Error) -> ConfigError {
    ConfigError::Watch {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// How long to wait before re-reading the sources to confirm the operator has
/// finished writing. See [`reload_once`].
const SETTLE_RECHECK: Duration = Duration::from_millis(25);

/// One reload cycle: re-resolve and validate; publish on success, keep the
/// current config and count a rejection on failure.
///
/// A cycle whose result is byte-for-byte the config already live publishes
/// nothing and counts nothing — a touched-but-unchanged file, or an event for
/// an unrelated file in a watched directory, is not a new generation.
///
/// **Editing a config file in place is not atomic.** `std::fs::write`, a shell
/// `>` redirect and most config-management tools truncate first and write
/// after, so a reload that lands in that window reads an empty or half-written
/// file. Such a file often still parses — an empty TOML file always does — and
/// every absent key then silently takes its default, which would publish limits
/// the operator never wrote and count it as `applied`. Two guards stop that:
///
/// 1. A zero-length base file is a truncation in progress, never "the operator
///    wants all defaults". (At startup an empty file *is* taken at face value;
///    only a live edit can race with a writer.)
/// 2. The load has to come back identical twice, [`SETTLE_RECHECK`] apart, so a
///    file still being extended — a `config.d` fragment a script appends in
///    chunks, say — is not published mid-write.
///
/// Either guard tripping skips the cycle and counts nothing. The write that is
/// still in flight raises its own filesystem event, which drives the next
/// attempt.
fn reload_once(inputs: &LoadInputs, handle: &ConfigHandle) {
    let m = &horndb_metrics::metrics().config;
    let (base_path, _) = resolve_base_path(inputs);
    if base_path.metadata().is_ok_and(|md| md.len() == 0) {
        eprintln!(
            "config: {} is empty — treating it as a write in progress, not reloading",
            base_path.display()
        );
        return;
    }
    match load(inputs) {
        Ok(new) => {
            std::thread::sleep(SETTLE_RECHECK);
            if load(inputs).ok().as_ref() != Some(&new) {
                eprintln!("config: sources changed while being read — not reloading yet");
                return;
            }
            let old = handle.current();
            if *old == new {
                return;
            }
            for key in restart_only_changes(&old, &new) {
                eprintln!("config: {key} changed — requires restart to take effect");
            }
            let generation = handle.publish(new);
            m.reloads
                .get_or_create(&ReloadResultLabel {
                    result: ReloadResult::Applied,
                })
                .inc();
            m.active_generation.set(generation as i64);
            m.last_reload_unixtime.set(unix_now());
            eprintln!("config: reload applied, generation {generation}");
        }
        Err(e) => {
            m.reloads
                .get_or_create(&ReloadResultLabel {
                    result: ReloadResult::Rejected,
                })
                .inc();
            eprintln!(
                "config: reload rejected, keeping generation {}: {e}",
                handle.generation()
            );
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::HumanDuration;
    use std::time::Duration;

    #[test]
    fn handle_starts_at_generation_one_and_increments() {
        let handle = ConfigHandle::default();
        assert_eq!(handle.generation(), 1);
        let mut cfg = ServerConfig::default();
        cfg.logging.level = "debug".to_string();
        assert_eq!(handle.publish(cfg), 2);
        assert_eq!(handle.current().logging.level, "debug");
    }

    #[test]
    fn hot_keys_are_not_reported_as_restart_only() {
        let old = ServerConfig::default();
        let mut new = old.clone();
        new.server.limits.max_result_rows = 5;
        new.logging.level = "debug".to_string();
        new.reload.debounce = HumanDuration(Duration::from_millis(10));
        assert!(restart_only_changes(&old, &new).is_empty());
    }

    #[test]
    fn bind_and_simd_are_restart_only() {
        let old = ServerConfig::default();
        let mut new = old.clone();
        new.server.bind = "0.0.0.0:9999".to_string();
        new.simd.autotune = !old.simd.autotune;
        assert_eq!(
            restart_only_changes(&old, &new),
            vec!["[server].bind", "[simd]"]
        );
    }
}
