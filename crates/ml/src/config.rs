//! Configuration for the ML integration boundary (SPEC-08 NF1).
//!
//! The `enabled` flag is the master switch. With `enabled = false`,
//! the [`crate::registry::MlRegistry`] hands out the `Disabled*`
//! implementations and the engine behaves bit-identically to a
//! non-ML build.

#[derive(Debug, Clone, PartialEq)]
pub struct MlConfig {
    pub enabled: bool,
}

impl MlConfig {
    pub fn disabled() -> Self {
        MlConfig { enabled: false }
    }
    pub fn enabled() -> Self {
        MlConfig { enabled: true }
    }
}

impl Default for MlConfig {
    /// Default is **disabled** — opt-in by design (SPEC-08 NF1).
    fn default() -> Self {
        Self::disabled()
    }
}

/// Errors raised by configuration / registration operations.
///
/// Reserved for future use — Stage 0/1 has no failure modes on
/// `MlRegistry::register_*` because registration is allowed
/// regardless of `enabled`, and the enabled flag only gates
/// *accessors*. Kept here so consumers can `use reasoner_ml::MlConfigError`
/// without breakage when Stage 2 adds e.g. an "invalid model id"
/// variant.
#[derive(Debug, thiserror::Error)]
pub enum MlConfigError {
    /// Placeholder — never returned in Stage 0/1. Documented here so
    /// the enum is non-empty and `match` exhaustiveness compiles.
    #[error("ml-config: unspecified configuration error")]
    Unspecified,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        assert!(!MlConfig::default().enabled);
    }

    #[test]
    fn explicit_constructors() {
        assert!(MlConfig::enabled().enabled);
        assert!(!MlConfig::disabled().enabled);
    }

    #[test]
    fn error_type_is_constructible() {
        // Lock the public surface so adding new variants doesn't
        // accidentally remove this one.
        let _e = MlConfigError::Unspecified;
    }
}
