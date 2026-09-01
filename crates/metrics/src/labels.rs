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

// Phases of a bulk load, timed separately so a slow load can be attributed
// (SPEC-17 §5.4.1). `Intern` is dictionary interning in `Store::apply_quads`
// — the term-based write path only; the id-based `Store::apply_quad_ids` the
// bulk loader uses interns nothing. The rest are inside
// `MemoryTier::insert_quad_batch`.
//
// `Dedupe` covers `HornBackend::insert_oxrdf_batch_in_graph`'s interning
// loop. It used to split into four `Dedupe*` sub-phases behind
// `HORNDB_DEDUPE_SUBPHASES=1` (HDB-90); that split existed to attribute the
// loop's cost against `intra_batch`, the in-batch dedup HDB-104 removed as
// redundant with `MemoryTier::apply_quad_batch`'s own per-predicate dedup —
// with `intra_batch` gone the loop is interning alone, so the split no
// longer has anything to attribute.
label_value_enum!(LoadPhase {
    Parse => "parse",
    Materialize => "materialize",
    Dedupe => "dedupe",
    Invalidate => "invalidate",
    Intern => "intern",
    Group => "group",
    CopyForward => "copy_forward",
    Merge => "merge",
    MergeRuns => "merge_runs",
    Build => "build",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct LoadPhaseLabel {
    pub phase: LoadPhase,
}

// Per-operator SPARQL execution-time phases (HDB-99), splitting the single
// `exec` pipeline stage so a slow query can be attributed to the operator
// that actually spent the time. Emitted only when `HORNDB_EXEC_PHASES=1` is
// set (see `crates/sparql/src/exec/phases.rs`); `docs/metrics.md`'s
// "SPARQL execution-time phases" section documents what each value covers.
// `Residual` is derived (`exec_elapsed - sum(the other 12)`), never clocked
// directly.
label_value_enum!(ExecPhase {
    ScanWcoj => "scan_wcoj",
    ScanRowBuild => "scan_row_build",
    ScanProvenance => "scan_provenance",
    JoinBuild => "join_build",
    JoinProbe => "join_probe",
    GroupKey => "group_key",
    GroupDecode => "group_decode",
    AggFold => "agg_fold",
    Sort => "sort",
    StreamOp => "stream_op",
    ResultEncode => "result_encode",
    Clock => "clock",
    Residual => "residual",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ExecPhaseLabel {
    pub phase: ExecPhase,
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
