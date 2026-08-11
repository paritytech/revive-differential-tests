//! The opcode-profile summary embedded in the report. Per-tx profiles and the
//! opcode catalog reuse the shared `revive_dt_common::profile` types directly;
//! only the cross-tx rollup (`AggregatedOpcode`) is report-specific.

use std::fmt;

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

/// A rollup row's opcode, or the synthetic `Other` bucket beyond the top-N.
/// Serializes to the opcode string or `"Other"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum AggregatedOpKey {
    Op(OpKey),
    Other,
}

impl fmt::Display for AggregatedOpKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggregatedOpKey::Op(op) => op.fmt(f),
            AggregatedOpKey::Other => f.write_str("Other"),
        }
    }
}

impl From<AggregatedOpKey> for String {
    fn from(key: AggregatedOpKey) -> Self {
        key.to_string()
    }
}

impl TryFrom<String> for AggregatedOpKey {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Ok(match s.as_str() {
            "Other" => AggregatedOpKey::Other,
            _ => AggregatedOpKey::Op(s.parse()?),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AggregatedOpcode {
    pub op: AggregatedOpKey,
    pub count: u64,
    pub total_ref_time: u128,
    pub total_proof_size: u128,
}
