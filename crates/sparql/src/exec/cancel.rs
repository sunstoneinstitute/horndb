//! The current query's cancellation token (SPEC-26 S5).
//!
//! **Thread-local, for the same reason [`crate::exec::phases`] is.**
//! `Executor::scan_bgp_ids` is an `&self` method on a backend shared across
//! query threads behind an `Arc<RwLock<_>>`, so the token can live neither on
//! the backend (every concurrent query would share one) nor on the `Runtime`
//! (it would have to be a parameter of every `Executor` method). The server
//! runs one query per blocking-pool thread, so a thread-local *is* the
//! per-query scope.
//!
//! `horndb-wcoj` gains no config dependency from this: the timer that trips
//! the token lives in `crates/sparql`'s server layer, and `wcoj` only ever
//! sees a plain [`CancelToken`].

use horndb_wcoj::cancel::CancelToken;
use std::cell::RefCell;

thread_local! {
    static CURRENT: RefCell<CancelToken> = RefCell::new(CancelToken::new());
}

/// The token to hand to executors on this thread. Outside a [`scope`] this
/// is a fresh, never-cancelled token — the pre-SPEC-26 behavior.
pub fn current() -> CancelToken {
    CURRENT.with(|c| c.borrow().clone())
}

/// Install `token` as [`current`] until the returned guard drops.
///
/// The guard resets rather than restores: query scopes never nest, and
/// blocking-pool threads are reused, so leaving a finished (possibly
/// cancelled) query's token installed would cancel the *next* query to land
/// on this thread — the same thread-reuse hazard `phases::reset` handles.
pub fn scope(token: CancelToken) -> Scope {
    CURRENT.with(|c| *c.borrow_mut() = token);
    Scope
}

/// Guard returned by [`scope`].
#[must_use = "the scope ends when this guard drops"]
pub struct Scope;

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = CancelToken::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_is_uncancelled_outside_a_scope() {
        assert!(!current().is_cancelled());
    }

    #[test]
    fn scope_publishes_the_token_and_resets_on_drop() {
        let token = CancelToken::new();
        {
            let _guard = scope(token.clone());
            token.cancel();
            assert!(current().is_cancelled(), "executors see the query's token");
        }
        assert!(
            !current().is_cancelled(),
            "a reused thread must not inherit the previous query's cancel"
        );
    }
}
