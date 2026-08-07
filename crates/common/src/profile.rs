//! Shared opcode-profiling model — one serializable set of types used by the
//! node connector (producer), the core aggregator, and the report (consumer).

use std::{collections::BTreeMap, fmt, str::FromStr};

use alloy::primitives::{BlockNumber, TxHash};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};

use crate::subscriptions::StepPath;

/// Genuine `(ref_time, proof_size)` weight; re-exported so the workspace shares
/// one `Weight` type.
pub use sp_weights::Weight;

/// Identifier of one opcode kind, distinguishing EVM opcodes from PVM syscalls.
///
/// Serializes to the stable string form `"EVMOpcode:0x52"` / `"PVMSyscall:0x03"`
/// so JSON consumers can parse the kind and byte without extra metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum OpKey {
    Evm(u8),
    Pvm(u8),
}

impl fmt::Display for OpKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpKey::Evm(b) => write!(f, "EVMOpcode:0x{b:02x}"),
            OpKey::Pvm(b) => write!(f, "PVMSyscall:0x{b:02x}"),
        }
    }
}

impl From<OpKey> for String {
    fn from(key: OpKey) -> Self {
        key.to_string()
    }
}

impl FromStr for OpKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, hex) = s
            .split_once(":0x")
            .ok_or_else(|| format!("malformed OpKey: {s}"))?;
        let byte =
            u8::from_str_radix(hex, 16).map_err(|e| format!("malformed OpKey byte {s}: {e}"))?;
        match kind {
            "EVMOpcode" => Ok(OpKey::Evm(byte)),
            "PVMSyscall" => Ok(OpKey::Pvm(byte)),
            _ => Err(format!("unknown OpKey kind: {s}")),
        }
    }
}

impl TryFrom<String> for OpKey {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Editorial opcode category. `Display`/serde use the human-readable strings so
/// the report and HTML share one vocabulary; `EnumIter` provides the canonical
/// display order (see [`Category::display_order`]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString, EnumIter,
)]
#[serde(into = "String", try_from = "String")]
pub enum Category {
    Storage,
    Calls,
    Returns,
    Memory,
    Stack,
    #[strum(serialize = "Arithmetic & Logic")]
    ArithmeticLogic,
    #[strum(serialize = "Control flow")]
    ControlFlow,
    Context,
    #[strum(serialize = "Calldata / Returndata")]
    CalldataReturndata,
    Code,
    Logs,
    Crypto,
    Immutables,
    #[strum(serialize = "VM overhead")]
    VmOverhead,
    Other,
}

impl From<Category> for String {
    fn from(category: Category) -> Self {
        category.to_string()
    }
}

impl TryFrom<String> for Category {
    type Error = strum::ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl Category {
    /// The categories in declaration order.
    pub fn display_order() -> Vec<Category> {
        Self::iter().collect()
    }
}

/// `byte → {name, category}` catalogs for EVM opcodes and PVM syscalls, embedded
/// in the report so consumers (including the HTML) don't ship their own tables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeCatalog {
    #[serde(default)]
    pub evm: BTreeMap<u8, OpcodeEntry>,
    #[serde(default)]
    pub pvm: BTreeMap<u8, OpcodeEntry>,
    /// Display order for non-Rust consumers (the HTML), which can't derive it
    /// from [`Category`]. Filled via [`Category::display_order`].
    #[serde(default)]
    pub category_order: Vec<Category>,
}

impl OpcodeCatalog {
    pub fn is_empty(&self) -> bool {
        self.evm.is_empty() && self.pvm.is_empty() && self.category_order.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeEntry {
    pub name: String,
    pub category: Category,
}

/// Per-opcode aggregate across one transaction's steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeStat {
    pub op: OpKey,
    pub count: u64,
    pub weight: Weight,
}

/// The weight accounting for one profiled tx. `unattributed = consumed − Σ step
/// weights` — the weight consumed but not attributed to any traced step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxWeights {
    pub consumed: Weight,
    pub base_call: Weight,
    pub unattributed: Weight,
}

/// Profile of one watched transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxProfile {
    pub tx_hash: TxHash,
    pub step_path: StepPath,
    /// Where the tx was included: block number, and its index within that block.
    pub block_number: BlockNumber,
    pub extrinsic_index: u32,
    pub failed: bool,
    pub gas_used: u64,
    pub weights: TxWeights,
    pub opcodes: Vec<OpcodeStat>,
}
