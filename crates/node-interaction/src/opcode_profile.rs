//! Opcode-profile types and aggregation.
//!
//! Two responsibilities:
//! 1. [`OpcodeCatalog`] snapshots upstream byte→name tables (revm for EVM,
//!    pallet-revive for PVM) and tags each with an editorial category. Carried
//!    in the report so consumers don't ship their own opcode tables.
//! 2. [`TxProfile::from_execution_trace`] aggregates one `ExecutionTrace`
//!    (returned by [`NodeApi::trace_execution_tx`](crate::NodeApi)) into
//!    per-opcode weight buckets + the unattributed-weight residual.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use alloy::primitives::TxHash;
use revive_dt_common::subscriptions::StepPath;

use crate::revive_metadata::runtime_types::pallet_revive::evm::api::debug_rpc_types::{
    ExecutionStepKind, ExecutionTrace,
};

/// `byte → {name, category}` catalogs for EVM opcodes and PVM syscalls.
/// Names come from upstream (revm + pallet-revive); categories are the
/// editorial taxonomy in [`categorize`]. Embedded in the report so HTML
/// consumers don't ship their own opcode tables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpcodeCatalog {
    pub evm: BTreeMap<u8, OpcodeEntry>,
    pub pvm: BTreeMap<u8, OpcodeEntry>,
    pub category_order: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpcodeEntry {
    pub name: String,
    pub category: &'static str,
}

pub const CATEGORY_ORDER: &[&str] = &[
    "Storage", "Calls", "Returns", "Memory", "Stack",
    "Arithmetic & Logic", "Control flow", "Context",
    "Calldata / Returndata", "Code", "Logs", "Crypto",
    "Immutables", "VM overhead", "Other",
];

/// Unknown names fall into `"Other"` — surfaces in the UI so a new opcode
/// upstream is visible until categorized here.
fn categorize(name: &str) -> &'static str {
    match name {
        "STOP" | "RETURN" | "REVERT" | "INVALID" | "SELFDESTRUCT" => "Returns",
        "ADD" | "MUL" | "SUB" | "DIV" | "SDIV" | "MOD" | "SMOD" | "ADDMOD"
        | "MULMOD" | "EXP" | "SIGNEXTEND" | "LT" | "GT" | "SLT" | "SGT" | "EQ"
        | "ISZERO" | "AND" | "OR" | "XOR" | "NOT" | "BYTE" | "SHL" | "SHR"
        | "SAR" => "Arithmetic & Logic",
        "KECCAK256" => "Crypto",
        "ADDRESS" | "BALANCE" | "ORIGIN" | "CALLER" | "CALLVALUE" | "GASPRICE"
        | "BLOCKHASH" | "COINBASE" | "TIMESTAMP" | "NUMBER" | "DIFFICULTY"
        | "GASLIMIT" | "CHAINID" | "SELFBALANCE" | "BASEFEE" | "BLOBHASH"
        | "BLOBBASEFEE" => "Context",
        "CALLDATALOAD" | "CALLDATASIZE" | "CALLDATACOPY" | "RETURNDATASIZE"
        | "RETURNDATACOPY" => "Calldata / Returndata",
        "CODESIZE" | "CODECOPY" | "EXTCODESIZE" | "EXTCODECOPY" | "EXTCODEHASH" => "Code",
        "POP" => "Stack",
        "MLOAD" | "MSTORE" | "MSTORE8" | "MSIZE" | "MCOPY" => "Memory",
        "SLOAD" | "SSTORE" | "TLOAD" | "TSTORE" => "Storage",
        "JUMP" | "JUMPI" | "PC" | "GAS" | "JUMPDEST" => "Control flow",
        "CREATE" | "CALL" | "CALLCODE" | "DELEGATECALL" | "CREATE2" | "STATICCALL" => "Calls",
        n if n.starts_with("PUSH") || n.starts_with("DUP") || n.starts_with("SWAP") => "Stack",
        n if n.starts_with("LOG") => "Logs",

        "set_storage" | "set_storage_or_clear" | "get_storage" | "get_storage_or_zero" => "Storage",
        "call" | "call_evm" | "delegate_call" | "delegate_call_evm" | "instantiate" => "Calls",
        "seal_return" | "terminate" | "consume_all_gas" => "Returns",
        "caller" | "origin" | "address" | "balance" | "balance_of" | "chain_id"
        | "gas_limit" | "value_transferred" | "gas_price" | "base_fee" | "now"
        | "block_number" | "block_hash" | "block_author" => "Context",
        "call_data_size" | "call_data_copy" | "call_data_load"
        | "return_data_size" | "return_data_copy" => "Calldata / Returndata",
        "code_hash" | "code_size" => "Code",
        "deposit_event" => "Logs",
        "hash_keccak_256" | "ecdsa_to_eth_address" | "sr25519_verify" => "Crypto",
        "get_immutable_data" | "set_immutable_data" => "Immutables",
        "noop" | "pvm_fuel" | "ref_time_left" => "VM overhead",

        _ => "Other",
    }
}

impl OpcodeCatalog {
    pub fn current() -> Self {
        let evm = (0..=u8::MAX)
            .filter_map(|byte| {
                let name = revm_bytecode::opcode::OpCode::name_by_op(byte);
                (name != "Unknown").then(|| {
                    (byte, OpcodeEntry { name: name.to_string(), category: categorize(name) })
                })
            })
            .collect();

        let mut pvm = BTreeMap::new();
        for byte in 0..=u8::MAX {
            let kind = pallet_revive::evm::ExecutionStepKind::PVMSyscall {
                op: byte,
                args: Vec::new(),
                returned: None,
            };
            let Ok(value) = serde_json::to_value(&kind) else { break };
            let Some(name) = value.get("op").and_then(|v| v.as_str()) else { break };
            pvm.insert(byte, OpcodeEntry { name: name.to_string(), category: categorize(name) });
        }

        Self { evm, pvm, category_order: CATEGORY_ORDER }
    }
}

/// Identifier of one opcode kind, distinguishing EVM opcodes from PVM
/// syscalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKey {
    EvmOpcode(u8),
    PvmSyscall(u8),
}

impl OpKey {
    /// Stable display form: `"EVMOpcode:0x52"`, `"PVMSyscall:0x03"`.
    pub fn as_string(&self) -> String {
        match self {
            OpKey::EvmOpcode(b) => format!("EVMOpcode:0x{:02x}", b),
            OpKey::PvmSyscall(b) => format!("PVMSyscall:0x{:02x}", b),
        }
    }
}

impl Ord for OpKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (OpKey::EvmOpcode(a), OpKey::EvmOpcode(b)) => a.cmp(b),
            (OpKey::PvmSyscall(a), OpKey::PvmSyscall(b)) => a.cmp(b),
            (OpKey::EvmOpcode(_), OpKey::PvmSyscall(_)) => Ordering::Less,
            (OpKey::PvmSyscall(_), OpKey::EvmOpcode(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for OpKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Per-opcode aggregate across one transaction's `struct_logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpcodeProfile {
    pub op_key: OpKey,
    pub count: u64,
    pub total_ref_time: u128,
    pub total_proof_size: u128,
}

/// Profile of one watched transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxProfile {
    pub tx_hash: TxHash,
    pub step_path: StepPath,
    pub failed: bool,
    pub gas_used: u64,
    pub weight_consumed_ref_time: u64,
    pub weight_consumed_proof_size: u64,
    pub base_call_weight_ref_time: u64,
    pub base_call_weight_proof_size: u64,
    pub opcodes: Vec<OpcodeProfile>,
    pub unattributed_ref_time: i128,
    pub unattributed_proof_size: i128,
}

impl TxProfile {
    /// Pure transform from `ExecutionTrace` to `TxProfile`.
    pub fn from_execution_trace(
        tx_hash: TxHash,
        step_path: StepPath,
        trace: &ExecutionTrace,
    ) -> Self {
        let mut by_op: HashMap<OpKey, (u64, u128, u128)> = HashMap::new();
        let mut step_total_ref_time: u128 = 0;
        let mut step_total_proof_size: u128 = 0;

        for step in &trace.struct_logs {
            let key = match step.kind {
                ExecutionStepKind::EVMOpcode { op, .. } => OpKey::EvmOpcode(op),
                ExecutionStepKind::PVMSyscall { op, .. } => OpKey::PvmSyscall(op),
            };
            let entry = by_op.entry(key).or_insert((0, 0, 0));
            entry.0 += 1;
            entry.1 += step.weight_cost.ref_time as u128;
            entry.2 += step.weight_cost.proof_size as u128;
            step_total_ref_time += step.weight_cost.ref_time as u128;
            step_total_proof_size += step.weight_cost.proof_size as u128;
        }

        let mut opcodes: Vec<OpcodeProfile> = by_op
            .into_iter()
            .map(|(op_key, (count, total_ref_time, total_proof_size))| OpcodeProfile {
                op_key,
                count,
                total_ref_time,
                total_proof_size,
            })
            .collect();
        opcodes.sort_by(|a, b| {
            b.total_ref_time
                .cmp(&a.total_ref_time)
                .then_with(|| a.op_key.cmp(&b.op_key))
        });

        let unattributed_ref_time =
            i128::from(trace.weight_consumed.ref_time) - step_total_ref_time as i128;
        let unattributed_proof_size =
            i128::from(trace.weight_consumed.proof_size) - step_total_proof_size as i128;

        Self {
            tx_hash,
            step_path,
            failed: trace.failed,
            gas_used: trace.gas,
            weight_consumed_ref_time: trace.weight_consumed.ref_time,
            weight_consumed_proof_size: trace.weight_consumed.proof_size,
            base_call_weight_ref_time: trace.base_call_weight.ref_time,
            base_call_weight_proof_size: trace.base_call_weight.proof_size,
            opcodes,
            unattributed_ref_time,
            unattributed_proof_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_catalog_resolves_names_and_categories() {
        let catalog = OpcodeCatalog::current();
        // EVM names from revm-bytecode + categories from `categorize`.
        let mstore = catalog.evm.get(&0x52).expect("0x52 in EVM");
        assert_eq!(mstore.name, "MSTORE");
        assert_eq!(mstore.category, "Memory");
        let call = catalog.evm.get(&0xf1).expect("0xf1 in EVM");
        assert_eq!(call.name, "CALL");
        assert_eq!(call.category, "Calls");
        let push7 = catalog.evm.get(&0x66).expect("0x66 in EVM");
        assert_eq!(push7.name, "PUSH7");
        assert_eq!(push7.category, "Stack");
        assert!(catalog.evm.get(&0x0c).is_none(), "0x0c is unassigned in EVM");
        // PVM names from pallet-revive's serde adapter.
        let set_storage = catalog.pvm.get(&0x01).expect("0x01 in PVM");
        assert_eq!(set_storage.name, "set_storage");
        assert_eq!(set_storage.category, "Storage");
        let pvm_fuel = catalog.pvm.get(&0x29).expect("0x29 in PVM");
        assert_eq!(pvm_fuel.name, "pvm_fuel");
        assert_eq!(pvm_fuel.category, "VM overhead");
        assert!(catalog.pvm.get(&0x2a).is_none(), "past end of list_trace_ops");
        assert!(catalog.pvm.len() >= 42);
        // Display order ends with the catch-all bucket.
        assert_eq!(catalog.category_order.last().copied(), Some("Other"));
    }

    use crate::revive_metadata::runtime_types::pallet_revive::evm::api::byte::Bytes;
    use crate::revive_metadata::runtime_types::pallet_revive::evm::api::debug_rpc_types::{
        ExecutionStep, ExecutionStepKind, ExecutionTrace,
    };
    use crate::revive_metadata::runtime_types::sp_weights::weight_v2::Weight;

    fn weight(ref_time: u64, proof_size: u64) -> Weight {
        Weight { ref_time, proof_size }
    }

    fn evm_step(op: u8, ref_time: u64, proof_size: u64) -> ExecutionStep {
        ExecutionStep {
            gas: 0,
            gas_cost: 0,
            weight_cost: weight(ref_time, proof_size),
            depth: 1,
            return_data: Bytes(Vec::new()),
            error: None,
            kind: ExecutionStepKind::EVMOpcode {
                pc: 0,
                op,
                stack: Vec::new(),
                memory: Vec::new(),
                storage: None,
            },
        }
    }

    fn pvm_step(op: u8, ref_time: u64, proof_size: u64) -> ExecutionStep {
        ExecutionStep {
            gas: 0,
            gas_cost: 0,
            weight_cost: weight(ref_time, proof_size),
            depth: 1,
            return_data: Bytes(Vec::new()),
            error: None,
            kind: ExecutionStepKind::PVMSyscall {
                op,
                args: Vec::new(),
                returned: None,
            },
        }
    }

    fn trace(
        base: Weight,
        consumed: Weight,
        failed: bool,
        steps: Vec<ExecutionStep>,
    ) -> ExecutionTrace {
        ExecutionTrace {
            gas: 0,
            weight_consumed: consumed,
            base_call_weight: base,
            failed,
            return_value: Bytes(Vec::new()),
            struct_logs: steps,
        }
    }

    #[test]
    fn repeated_opcode_accumulates_count_and_weight() {
        let t = trace(
            weight(0, 0),
            weight(300, 30),
            false,
            vec![
                evm_step(0x01, 100, 10), // ADD
                evm_step(0x01, 100, 10),
                evm_step(0x01, 100, 10),
            ],
        );
        let p = TxProfile::from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), &t);
        assert_eq!(p.opcodes.len(), 1);
        assert_eq!(p.opcodes[0].count, 3);
        assert_eq!(p.opcodes[0].total_ref_time, 300);
        assert_eq!(p.opcodes[0].total_proof_size, 30);
    }

    #[test]
    fn evm_and_pvm_kept_separate_sorted_by_ref_time() {
        let t = trace(
            weight(0, 0),
            weight(0, 0), // not relevant — we test ordering, not residual
            false,
            vec![
                evm_step(0x01, 50, 0),   // ADD: small
                pvm_step(0x03, 500, 0),  // big PVM syscall
                evm_step(0x52, 200, 0),  // MSTORE: medium
                pvm_step(0x03, 500, 0),  // same PVM syscall again → 1000 total
            ],
        );
        let p = TxProfile::from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), &t);
        assert_eq!(p.opcodes.len(), 3);
        assert_eq!(p.opcodes[0].op_key, OpKey::PvmSyscall(0x03));
        assert_eq!(p.opcodes[0].count, 2);
        assert_eq!(p.opcodes[0].total_ref_time, 1000);
        assert_eq!(p.opcodes[1].op_key, OpKey::EvmOpcode(0x52));
        assert_eq!(p.opcodes[2].op_key, OpKey::EvmOpcode(0x01));
    }

    #[test]
    fn unattributed_residual_captures_overhead() {
        // weight_consumed = 2000, sum of step weights = 1500.
        // Residual = 2000 - 1500 = 500.
        let t = trace(
            weight(100, 5),
            weight(2000, 50),
            false,
            vec![evm_step(0x01, 1000, 20), evm_step(0x52, 500, 15)],
        );
        let p = TxProfile::from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), &t);
        assert_eq!(p.unattributed_ref_time, 500);
        assert_eq!(p.unattributed_proof_size, 15);
    }

    #[test]
    fn op_key_display_format() {
        assert_eq!(OpKey::EvmOpcode(0x52).as_string(), "EVMOpcode:0x52");
        assert_eq!(OpKey::PvmSyscall(0x03).as_string(), "PVMSyscall:0x03");
    }
}
