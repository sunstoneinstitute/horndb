//! Hot-set advisor for tier placement (SPEC-08 F4).
//!
//! Predicts which triples will be queried frequently in the next
//! window. SPEC-02's tiering uses this as one input to placement
//! (alongside actual recent-access statistics).
//!
//! Triple IDs are opaque `u64`s here — SPEC-02 owns their meaning;
//! we just shuttle them.

use crate::types::ModelId;

/// Opaque triple identifier from SPEC-02 storage.
///
/// Defined here as a type alias so consumers don't need to import a
/// storage type just to implement this trait.
pub type TripleId = u64;

pub trait HotSetAdvisor: Send + Sync {
    fn model_id(&self) -> ModelId;

    /// Return up to `max` triple IDs predicted to be hot in the
    /// upcoming window. May return fewer; may return an empty Vec.
    fn predict_hot(&self, max: usize) -> Vec<TripleId>;
}

#[derive(Debug, Default)]
pub struct DisabledHotSetAdvisor;

impl DisabledHotSetAdvisor {
    pub const MODEL_ID: &'static str = "disabled-hotset-advisor";
}

impl HotSetAdvisor for DisabledHotSetAdvisor {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }
    fn predict_hot(&self, _max: usize) -> Vec<TripleId> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty() {
        assert!(DisabledHotSetAdvisor.predict_hot(1000).is_empty());
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        assert_eq!(
            DisabledHotSetAdvisor.model_id().as_str(),
            "disabled-hotset-advisor"
        );
    }

    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn HotSetAdvisor> = Box::new(DisabledHotSetAdvisor);
    }
}
