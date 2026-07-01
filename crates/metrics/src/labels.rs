//! Typed label sets and values. No strings at call sites.
//!
//! The `prometheus-client` `EncodeLabelValue` derive (v0.23) emits the Rust
//! variant name verbatim (`Query`) with no rename attribute, but our metric
//! contract requires lowercase label values (`endpoint="query"`). We therefore
//! implement `EncodeLabelValue` by hand for each enum, mapping every variant to
//! its lowercase string. `EncodeLabelSet` (for the label-set structs) is still
//! derived — it only governs the label *keys*, which already match the field
//! names.
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue, LabelValueEncoder};

/// Implement `EncodeLabelValue` for a fieldless enum by mapping each variant to
/// a lowercase string literal.
macro_rules! label_value_enum {
    ($name:ident { $($variant:ident => $repr:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, Hash, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $repr),+
                }
            }
        }

        impl EncodeLabelValue for $name {
            fn encode(&self, encoder: &mut LabelValueEncoder) -> Result<(), std::fmt::Error> {
                // Fully-qualified so the macro is hygienic — it does not depend
                // on `std::fmt::Write` being imported at the expansion site.
                core::fmt::Write::write_str(encoder, self.as_str())
            }
        }
    };
}

label_value_enum!(Endpoint {
    Query => "query",
    Update => "update",
    Metrics => "metrics",
});

label_value_enum!(Method {
    Get => "get",
    Post => "post",
});

label_value_enum!(QueryKind {
    Select => "select",
    Ask => "ask",
    Construct => "construct",
    Describe => "describe",
    Update => "update",
});

label_value_enum!(Stage {
    Parse => "parse",
    Translate => "translate",
    Plan => "plan",
    Exec => "exec",
});

label_value_enum!(MemTier {
    Dram => "dram",
    Hbm => "hbm",
    Cxl => "cxl",
    Unknown => "unknown",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    pub endpoint: Endpoint,
    pub method: Method,
    pub status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct EndpointLabel {
    pub endpoint: Endpoint,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct QueryKindLabel {
    pub kind: QueryKind,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StageLabel {
    pub stage: Stage,
}

label_value_enum!(Phase {
    CompiledRules => "compiled_rules",
    ListRules => "list_rules",
    ClosureBackend => "closure_backend",
    Apply => "apply",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PhaseLabel {
    pub phase: Phase,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RuleLabel {
    pub rule: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TierLabel {
    pub tier: MemTier,
}

label_value_enum!(NlResult {
    Ok => "ok",
    Error => "error",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct NlResultLabel {
    pub result: NlResult,
}

label_value_enum!(SimdKernel {
    Intersect => "intersect",
    LowerBound => "lower_bound",
    Merge => "merge",
    Dedup => "dedup",
    FilterRange => "filter_range",
    FilterIndicesEq => "filter_indices_eq",
    Gather => "gather",
});

label_value_enum!(SimdIsa {
    Scalar => "scalar",
    Avx2 => "avx2",
    Avx512 => "avx512",
    Neon => "neon",
});

label_value_enum!(SimdSource {
    Table => "table",
    Calibrated => "calibrated",
    Static => "static",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SimdKernelLabel {
    pub kernel: SimdKernel,
    pub isa: SimdIsa,
    pub source: SimdSource,
}
