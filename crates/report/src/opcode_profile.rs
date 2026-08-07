//! The opcode-profile summary embedded in the report. Per-tx profiles and the
//! opcode catalog reuse the shared `revive_dt_common::profile` types directly;
//! only the cross-tx rollup (`AggregatedOpcode`) is report-specific.

use crate::internal_prelude::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpcodeProfileSummary {
    pub sampled_tx_count: usize,
    /// The number of distinct blocks the sampled transactions were included in
    /// (the coverage this profile reflects — not the whole run's block count).
    pub block_count: u32,
    /// Sampled txs whose tracer reported `failed = true`. Reverted txs are
    /// still aggregated below — they spent metered weight worth profiling.
    pub failed_count: usize,
    /// Sorted descending by `total_ref_time`; trailing rows beyond top-N
    /// collapse into a single `"Other"` entry.
    pub opcodes: Vec<AggregatedOpcode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tx_profiles: Vec<TxProfile>,
    #[serde(default, skip_serializing_if = "OpcodeCatalog::is_empty")]
    pub opcode_catalog: OpcodeCatalog,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AggregatedOpcode {
    pub op: String,
    pub count: u64,
    pub total_ref_time: u128,
    pub total_proof_size: u128,
}
