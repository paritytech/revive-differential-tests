//! Serializable opcode-profile types. Decoupled from `revive_dt_node_interaction`'s
//! `TxProfile` (which embeds subxt-generated, non-`Serialize` types).

use std::collections::BTreeMap;

use crate::internal_prelude::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpcodeProfileSummary {
    pub sampled_tx_count: usize,
    pub block_count: u32,
    /// Sampled txs whose tracer reported `failed = true`. Reverted txs are
    /// still aggregated below — they spent metered weight worth profiling.
    pub failed_count: usize,
    /// Sorted descending by `total_ref_time`; trailing rows beyond top-N
    /// collapse into a single `"Other"` entry.
    pub opcodes: Vec<AggregatedOpcode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tx_profiles: Vec<TxProfileWire>,
    #[serde(default, skip_serializing_if = "OpcodeCatalogWire::is_empty")]
    pub opcode_catalog: OpcodeCatalogWire,
}

/// Wire view of `revive_dt_node_interaction::OpcodeCatalog`. Byte keys are
/// JSON strings (`"0".."255"`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeCatalogWire {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evm: BTreeMap<String, OpcodeEntryWire>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pvm: BTreeMap<String, OpcodeEntryWire>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category_order: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeEntryWire {
    pub name: String,
    pub category: String,
}

impl OpcodeCatalogWire {
    pub fn is_empty(&self) -> bool {
        self.evm.is_empty() && self.pvm.is_empty() && self.category_order.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AggregatedOpcode {
    pub op_key: String,
    pub sample_count: u64,
    pub total_ref_time: u128,
    pub total_proof_size: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxProfileWire {
    pub tx_hash: TxHash,
    pub step_path: StepPath,
    pub failed: bool,
    pub gas_used: u64,
    pub weight_consumed_ref_time: u64,
    pub weight_consumed_proof_size: u64,
    pub base_call_weight_ref_time: u64,
    pub base_call_weight_proof_size: u64,
    pub unattributed_ref_time: i128,
    pub unattributed_proof_size: i128,
    pub opcodes: Vec<AggregatedOpcode>,
}
