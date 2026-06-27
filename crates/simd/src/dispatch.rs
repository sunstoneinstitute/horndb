//! ISA selection and the F5 test-only override.
//!
//! Production code resolves the ISA from CPU feature detection. Tests use
//! [`with_forced_isa`] to pin a path (scalar/AVX2/AVX-512/NEON) regardless of
//! the host, so every kernel the host *can* execute is exercised by the
//! differential proptests (SPEC-12 F5 / acceptance #1, #6).

/// Instruction-set path a primitive can dispatch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isa {
    Scalar,
    Avx2,
    Avx512,
    Neon,
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<Option<Isa>> = const { std::cell::Cell::new(None) };
}

/// The ISA a test has forced for the current thread, or `None` in production.
#[inline]
pub fn forced_isa() -> Option<Isa> {
    #[cfg(test)]
    {
        FORCED.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        None
    }
}

/// Run `f` with `isa` forced as the dispatch target on this thread. Restores
/// the previous value on return (even on panic — uses a drop guard).
///
/// Test-support API: only compiled under `#[cfg(test)]` and re-exported for the
/// differential proptests / benches that need to pin a specific ISA path.
#[cfg(test)]
pub fn with_forced_isa<R>(isa: Isa, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Isa>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|c| c.set(self.0));
        }
    }
    let prev = FORCED.with(|c| c.replace(Some(isa)));
    let _restore = Restore(prev);
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_isa_overrides_within_closure() {
        assert_eq!(forced_isa(), None);
        with_forced_isa(Isa::Scalar, || {
            assert_eq!(forced_isa(), Some(Isa::Scalar));
        });
        assert_eq!(
            forced_isa(),
            None,
            "override must not leak past the closure"
        );
    }
}
