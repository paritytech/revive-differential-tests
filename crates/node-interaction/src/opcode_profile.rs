//! Opcode-profile production: the opcode catalog and the trace→profile transform.
//!
//! Two responsibilities:
//! 1. [`current_catalog`] snapshots upstream byte→name tables (revm for EVM,
//!    pallet-revive for PVM) and tags each with an editorial `Category`.
//! 2. [`from_execution_trace`] aggregates one `ExecutionTrace`
//!    (returned by [`NodeApi::trace_execution_tx`](crate::NodeApi)) into the
//!    shared [`TxProfile`] — per-opcode weight buckets + the unattributed-weight
//!    residual.

use std::collections::{BTreeMap, HashMap};

use alloy::primitives::{BlockNumber, TxHash};
use pallet_revive_types::runtime_api::{ExecutionStepKindV1, ExecutionTraceV1, PolkavmSyscallV1};
use revive_dt_common::profile::{
    Category, OpKey, OpcodeCatalog, OpcodeEntry, OpcodeStat, TxProfile, TxWeights, Weight,
};
use revive_dt_common::subscriptions::StepPath;
use strum::VariantArray;

/// Categorize an EVM opcode by revm name (revm has no opcode enum). Unknown
/// names fall into [`Category::Other`].
fn categorize_evm(name: &str) -> Category {
    match name {
        "STOP" | "RETURN" | "REVERT" | "INVALID" | "SELFDESTRUCT" => Category::Returns,
        "ADD" | "MUL" | "SUB" | "DIV" | "SDIV" | "MOD" | "SMOD" | "ADDMOD" | "MULMOD" | "EXP"
        | "SIGNEXTEND" | "LT" | "GT" | "SLT" | "SGT" | "EQ" | "ISZERO" | "AND" | "OR" | "XOR"
        | "NOT" | "BYTE" | "SHL" | "SHR" | "SAR" => Category::ArithmeticLogic,
        "KECCAK256" => Category::Crypto,
        "ADDRESS" | "BALANCE" | "ORIGIN" | "CALLER" | "CALLVALUE" | "GASPRICE" | "BLOCKHASH"
        | "COINBASE" | "TIMESTAMP" | "NUMBER" | "DIFFICULTY" | "GASLIMIT" | "CHAINID"
        | "SELFBALANCE" | "BASEFEE" | "BLOBHASH" | "BLOBBASEFEE" => Category::Context,
        "CALLDATALOAD" | "CALLDATASIZE" | "CALLDATACOPY" | "RETURNDATASIZE" | "RETURNDATACOPY" => {
            Category::CalldataReturndata
        }
        "CODESIZE" | "CODECOPY" | "EXTCODESIZE" | "EXTCODECOPY" | "EXTCODEHASH" => Category::Code,
        "POP" => Category::Stack,
        "MLOAD" | "MSTORE" | "MSTORE8" | "MSIZE" | "MCOPY" => Category::Memory,
        "SLOAD" | "SSTORE" | "TLOAD" | "TSTORE" => Category::Storage,
        "JUMP" | "JUMPI" | "PC" | "GAS" | "JUMPDEST" => Category::ControlFlow,
        "CREATE" | "CALL" | "CALLCODE" | "DELEGATECALL" | "CREATE2" | "STATICCALL" => {
            Category::Calls
        }
        n if n.starts_with("PUSH") || n.starts_with("DUP") || n.starts_with("SWAP") => {
            Category::Stack
        }
        n if n.starts_with("LOG") => Category::Logs,
        _ => Category::Other,
    }
}

/// Categorize a PVM syscall. Exhaustive over [`PolkavmSyscallV1`], so a new
/// syscall upstream won't compile until it's categorized here.
fn categorize_pvm(syscall: &PolkavmSyscallV1) -> Category {
    use PolkavmSyscallV1::*;
    match syscall {
        SetStorage | SetStorageOrClear | GetStorage | GetStorageOrZero => Category::Storage,
        Call | CallEvm | DelegateCall | DelegateCallEvm | Instantiate => Category::Calls,
        SealReturn | Terminate | ConsumeAllGas => Category::Returns,
        Caller | Origin | Address | Balance | BalanceOf | ChainId | GasLimit | ValueTransferred
        | GasPrice | BaseFee | Now | BlockNumber | BlockHash | BlockAuthor => Category::Context,
        CallDataSize | CallDataCopy | CallDataLoad | ReturnDataSize | ReturnDataCopy => {
            Category::CalldataReturndata
        }
        CodeHash | CodeSize => Category::Code,
        DepositEvent => Category::Logs,
        HashKeccak256 | EcdsaToEthAddress | Sr25519Verify => Category::Crypto,
        GetImmutableData | SetImmutableData => Category::Immutables,
        Noop | PvmFuel | RefTimeLeft => Category::VmOverhead,
    }
}

/// Snapshot the current EVM-opcode (revm) and PVM-syscall (pallet-revive) name
/// tables, tagging each with its editorial [`Category`].
pub fn current_catalog() -> OpcodeCatalog {
    let evm = (0..=u8::MAX)
        .filter_map(|byte| {
            let name = revm_bytecode::opcode::OpCode::name_by_op(byte);
            (name != "Unknown").then(|| {
                (
                    byte,
                    OpcodeEntry {
                        name: name.to_string(),
                        category: categorize_evm(name),
                    },
                )
            })
        })
        .collect();

    // PVM syscall names come from the typed `PolkavmSyscallV1` enum (strum), keyed
    // by the enum's discriminant to match the `op` byte in execution traces.
    let mut pvm = BTreeMap::new();
    for syscall in PolkavmSyscallV1::VARIANTS {
        // Key by the `#[repr(u8)]` discriminant: the `op` byte traces carry.
        let byte = *syscall as u8;
        let name: &'static str = syscall.into();
        pvm.insert(
            byte,
            OpcodeEntry {
                name: name.to_string(),
                category: categorize_pvm(syscall),
            },
        );
    }

    OpcodeCatalog {
        evm,
        pvm,
        category_order: Category::display_order(),
    }
}

/// Per-opcode accumulator used while folding a trace's steps.
#[derive(Default)]
struct OpcodeStats {
    count: u64,
    ref_time: u64,
    proof_size: u64,
}

/// Pure transform from `ExecutionTrace` to the shared [`TxProfile`].
///
/// `block_number`/`extrinsic_index` locate the tx in the chain; the connector
/// resolves them while tracing (they aren't carried in the trace itself).
pub fn from_execution_trace(
    tx_hash: TxHash,
    step_path: StepPath,
    block_number: BlockNumber,
    extrinsic_index: u32,
    trace: &ExecutionTraceV1,
) -> TxProfile {
    let mut by_op = HashMap::<OpKey, OpcodeStats>::new();
    let mut step_total_ref_time: u64 = 0;
    let mut step_total_proof_size: u64 = 0;

    for step in &trace.struct_logs {
        let key = match &step.kind {
            ExecutionStepKindV1::EVMOpcode { op, .. } => OpKey::Evm(op.0),
            ExecutionStepKindV1::PVMSyscall { op, .. } => OpKey::Pvm(*op as u8),
        };
        let entry = by_op.entry(key).or_default();
        entry.count += 1;
        entry.ref_time += step.weight_cost.ref_time();
        entry.proof_size += step.weight_cost.proof_size();
        step_total_ref_time += step.weight_cost.ref_time();
        step_total_proof_size += step.weight_cost.proof_size();
    }

    let mut opcodes: Vec<OpcodeStat> = by_op
        .into_iter()
        .map(|(op, stats)| OpcodeStat {
            op,
            count: stats.count,
            weight: Weight::from_parts(stats.ref_time, stats.proof_size),
        })
        .collect();
    opcodes.sort_by(|a, b| {
        b.weight
            .ref_time()
            .cmp(&a.weight.ref_time())
            .then_with(|| a.op.cmp(&b.op))
    });

    let consumed = Weight::from_parts(
        trace.weight_consumed.ref_time(),
        trace.weight_consumed.proof_size(),
    );
    // Weight consumed but not attributed to any traced step. A trace's step
    // weights never exceed `weight_consumed`, so this is non-negative.
    let unattributed = consumed.saturating_sub(Weight::from_parts(
        step_total_ref_time,
        step_total_proof_size,
    ));

    TxProfile {
        tx_hash,
        step_path,
        block_number,
        extrinsic_index,
        failed: trace.failed,
        gas_used: trace.gas,
        weights: TxWeights {
            consumed,
            base_call: Weight::from_parts(
                trace.base_call_weight.ref_time(),
                trace.base_call_weight.proof_size(),
            ),
            unattributed,
        },
        opcodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_catalog_resolves_names_and_categories() {
        let catalog = current_catalog();
        // EVM names from revm-bytecode + categories from `categorize_evm`.
        let mstore = catalog.evm.get(&0x52).expect("0x52 in EVM");
        assert_eq!(mstore.name, "MSTORE");
        assert_eq!(mstore.category, Category::Memory);
        let call = catalog.evm.get(&0xf1).expect("0xf1 in EVM");
        assert_eq!(call.name, "CALL");
        assert_eq!(call.category, Category::Calls);
        let push7 = catalog.evm.get(&0x66).expect("0x66 in EVM");
        assert_eq!(push7.name, "PUSH7");
        assert_eq!(push7.category, Category::Stack);
        assert!(
            catalog.evm.get(&0x0c).is_none(),
            "0x0c is unassigned in EVM"
        );
        // PVM names from pallet-revive's serde adapter.
        let set_storage = catalog.pvm.get(&0x01).expect("0x01 in PVM");
        assert_eq!(set_storage.name, "set_storage");
        assert_eq!(set_storage.category, Category::Storage);
        let pvm_fuel = catalog.pvm.get(&0x29).expect("0x29 in PVM");
        assert_eq!(pvm_fuel.name, "pvm_fuel");
        assert_eq!(pvm_fuel.category, Category::VmOverhead);
        assert!(
            catalog.pvm.get(&0x2a).is_none(),
            "past end of list_trace_ops"
        );
        assert!(catalog.pvm.len() >= 42);
        // Display order ends with the catch-all bucket.
        assert_eq!(
            catalog.category_order.last().copied(),
            Some(Category::Other)
        );
    }

    use pallet_revive::Weight;
    use pallet_revive_types::common::Bytes;
    use pallet_revive_types::runtime_api::{
        EvmOpcodeV1, ExecutionStepKindV1, ExecutionStepV1, ExecutionTraceV1,
    };

    fn weight(ref_time: u64, proof_size: u64) -> Weight {
        Weight::from_parts(ref_time, proof_size)
    }

    fn evm_step(op: u8, ref_time: u64, proof_size: u64) -> ExecutionStepV1 {
        ExecutionStepV1 {
            gas: 0,
            gas_cost: 0,
            weight_cost: weight(ref_time, proof_size),
            depth: 1,
            return_data: Bytes(Vec::new()),
            error: None,
            kind: ExecutionStepKindV1::EVMOpcode {
                pc: 0,
                op: EvmOpcodeV1(op),
                stack: Vec::new(),
                memory: Vec::new(),
                storage: None,
            },
        }
    }

    fn pvm_step(op: u8, ref_time: u64, proof_size: u64) -> ExecutionStepV1 {
        ExecutionStepV1 {
            gas: 0,
            gas_cost: 0,
            weight_cost: weight(ref_time, proof_size),
            depth: 1,
            return_data: Bytes(Vec::new()),
            error: None,
            kind: ExecutionStepKindV1::PVMSyscall {
                op: PolkavmSyscallV1::VARIANTS[op as usize].clone(),
                args: Vec::new(),
                returned: None,
            },
        }
    }

    fn trace(
        base: Weight,
        consumed: Weight,
        failed: bool,
        steps: Vec<ExecutionStepV1>,
    ) -> ExecutionTraceV1 {
        ExecutionTraceV1 {
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
        let p = from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), 0, 0, &t);
        assert_eq!(p.opcodes.len(), 1);
        assert_eq!(p.opcodes[0].count, 3);
        assert_eq!(p.opcodes[0].weight.ref_time(), 300);
        assert_eq!(p.opcodes[0].weight.proof_size(), 30);
    }

    #[test]
    fn evm_and_pvm_kept_separate_sorted_by_ref_time() {
        let t = trace(
            weight(0, 0),
            weight(0, 0), // not relevant — we test ordering, not residual
            false,
            vec![
                evm_step(0x01, 50, 0),  // ADD: small
                pvm_step(0x03, 500, 0), // big PVM syscall
                evm_step(0x52, 200, 0), // MSTORE: medium
                pvm_step(0x03, 500, 0), // same PVM syscall again → 1000 total
            ],
        );
        let p = from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), 0, 0, &t);
        assert_eq!(p.opcodes.len(), 3);
        assert_eq!(p.opcodes[0].op, OpKey::Pvm(0x03));
        assert_eq!(p.opcodes[0].count, 2);
        assert_eq!(p.opcodes[0].weight.ref_time(), 1000);
        assert_eq!(p.opcodes[1].op, OpKey::Evm(0x52));
        assert_eq!(p.opcodes[2].op, OpKey::Evm(0x01));
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
        let p = from_execution_trace(TxHash::ZERO, StepPath::new(vec![]), 0, 0, &t);
        assert_eq!(p.weights.unattributed.ref_time(), 500);
        assert_eq!(p.weights.unattributed.proof_size(), 15);
    }

    #[test]
    fn op_key_display_format() {
        assert_eq!(OpKey::Evm(0x52).to_string(), "EVMOpcode:0x52");
        assert_eq!(OpKey::Pvm(0x03).to_string(), "PVMSyscall:0x03");
    }
}
