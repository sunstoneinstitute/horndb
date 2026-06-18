//! Configuration for the ML integration boundary (SPEC-08 NF1).
//!
//! The `enabled` flag is the master switch. With `enabled = false`,
//! the [`crate::registry::MlRegistry`] hands out the `Disabled*`
//! implementations and the engine behaves bit-identically to a
//! non-ML build.

#[derive(Debug, Clone, PartialEq)]
pub struct MlConfig {
    pub enabled: bool,
    /// Training-data leakage controls for the NL-query endpoint
    /// (SPEC-08 F3 / "Training-data leakage" risk).
    pub llm_privacy: LlmPrivacy,
}

impl MlConfig {
    pub fn disabled() -> Self {
        MlConfig {
            enabled: false,
            llm_privacy: LlmPrivacy::default(),
        }
    }
    pub fn enabled() -> Self {
        MlConfig {
            enabled: true,
            llm_privacy: LlmPrivacy::default(),
        }
    }

    pub fn with_privacy(mut self, p: LlmPrivacy) -> Self {
        self.llm_privacy = p;
        self
    }
}

impl Default for MlConfig {
    /// Default is **disabled** — opt-in by design (SPEC-08 NF1).
    fn default() -> Self {
        Self::disabled()
    }
}

/// Controls how much of an NL question may be retained or forwarded.
///
/// SPEC-08 calls out two risks that this addresses:
///
/// * **Training-data leakage** — questions sent to a hosted LLM may be
///   logged by the provider and used for training. Operators who cannot
///   accept that disable retention (`log_questions = false`) so the
///   engine never persists the raw text in its audit/telemetry.
/// * **Privacy / GDPR** — questions may carry PII. `redact_in_logs`
///   replaces the stored question text with a placeholder while still
///   recording that *a* query happened (and its cost), so audit volume
///   is preserved without the literal content.
///
/// The defaults are the privacy-preserving choice: do not retain raw
/// question text. An operator must explicitly opt in to logging.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmPrivacy {
    /// Persist the raw question text in audit/telemetry records.
    /// Default `false` — privacy-preserving.
    pub log_questions: bool,
    /// When a question *is* recorded, replace its text with a redaction
    /// placeholder. Default `true`. Set `false` (with `log_questions`)
    /// to retain the literal text.
    pub redact_in_logs: bool,
}

impl LlmPrivacy {
    /// Strictest posture: never retain question text.
    pub fn no_retention() -> Self {
        LlmPrivacy {
            log_questions: false,
            redact_in_logs: true,
        }
    }

    /// Retain literal question text (operator accepted the leakage risk).
    pub fn retain_questions() -> Self {
        LlmPrivacy {
            log_questions: true,
            redact_in_logs: false,
        }
    }

    /// Whether translator-provided free text (e.g. an `explanation`) may
    /// be echoed back to the caller.
    ///
    /// A third-party translator can put the raw question into such fields,
    /// and the endpoint cannot inspect arbitrary text for PII. So free
    /// text is only echoed when the policy permits retaining the literal
    /// question (`log_questions && !redact_in_logs`). Under no-retention
    /// or redaction it is suppressed — the structured fields
    /// (`generated_sparql`, `confidence`, `cost`) are unaffected.
    pub fn may_echo_free_text(&self) -> bool {
        self.log_questions && !self.redact_in_logs
    }

    /// Decide what text (if any) should be stored for `question`.
    ///
    /// Returns `None` when nothing should be retained, `Some(redacted)`
    /// or `Some(literal)` otherwise. This is the single chokepoint the
    /// endpoint calls before writing any telemetry, so the policy can
    /// never be bypassed by a forgetful call site.
    pub fn loggable_text(&self, question: &str) -> Option<String> {
        if !self.log_questions {
            return None;
        }
        if self.redact_in_logs {
            Some(format!("[redacted: {} chars]", question.chars().count()))
        } else {
            Some(question.to_string())
        }
    }
}

impl Default for LlmPrivacy {
    fn default() -> Self {
        Self::no_retention()
    }
}

/// Errors raised by configuration / registration operations.
///
/// Reserved for future use — Stage 0/1 has no failure modes on
/// `MlRegistry::register_*` because registration is allowed
/// regardless of `enabled`, and the enabled flag only gates
/// *accessors*. Kept here so consumers can `use horndb_ml::MlConfigError`
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

    #[test]
    fn default_privacy_retains_nothing() {
        let p = LlmPrivacy::default();
        assert!(!p.log_questions);
        assert_eq!(p.loggable_text("who is alice?"), None);
        // Config default also retains nothing.
        assert_eq!(MlConfig::default().llm_privacy, LlmPrivacy::no_retention());
    }

    #[test]
    fn retain_questions_keeps_literal_text() {
        let p = LlmPrivacy::retain_questions();
        assert_eq!(
            p.loggable_text("who is alice?"),
            Some("who is alice?".to_string())
        );
    }

    #[test]
    fn redaction_records_length_not_content() {
        let p = LlmPrivacy {
            log_questions: true,
            redact_in_logs: true,
        };
        // 5 chars, content not present in the output.
        let out = p.loggable_text("abcde").unwrap();
        assert_eq!(out, "[redacted: 5 chars]");
        assert!(!out.contains("abcde"));
    }

    #[test]
    fn with_privacy_overrides() {
        let c = MlConfig::enabled().with_privacy(LlmPrivacy::retain_questions());
        assert!(c.enabled);
        assert!(c.llm_privacy.log_questions);
    }

    #[test]
    fn may_echo_free_text_only_under_full_retention() {
        assert!(!LlmPrivacy::no_retention().may_echo_free_text());
        assert!(LlmPrivacy::retain_questions().may_echo_free_text());
        // Redaction on => suppress free text even if logging is enabled.
        assert!(!LlmPrivacy {
            log_questions: true,
            redact_in_logs: true,
        }
        .may_echo_free_text());
    }
}
