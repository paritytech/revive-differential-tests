//! Watched-tx opcode profiler.
//!
//! The pipeline runs in two passes so that per-workload state (the
//! [`TransactionFinder`]) doesn't have to survive past its workload's
//! lifetime:
//!
//! 1. **At the end of each `(platform, workload)`** (finder still alive):
//!    sample tx hashes per [`StepPath`] via [`sample_watched_txs`], resolve
//!    each sampled hash to `(tx_index_in_block, substrate_block)` via the
//!    finder, group by block hash → [`PerBlockTraceJob`]s.
//! 2. **At the end of all benchmarks** (nodes still alive): execute the
//!    accumulated jobs via [`execute_trace_jobs`] against each platform's
//!    node, aggregating to [`TxProfile`]s.

use std::collections::HashMap;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use revive_dt_node_interaction::opcode_profile::{OpcodeCatalog, TxProfile};
use revive_dt_node_interaction::revive_metadata::runtime_types::pallet_revive::evm::api::debug_rpc_types::ExecutionTracerConfig;
use revive_dt_report::{
    AggregatedOpcode, OpcodeCatalogWire, OpcodeEntryWire, OpcodeProfileSummary, TxProfileWire,
};
use subxt::utils::H256 as SubxtH256;
use tracing::warn;

use crate::differential_benchmarks::transaction_finder::TransactionFinder;
use crate::internal_prelude::*;

/// How to choose which watched transactions become profiler input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingMode {
    /// Pick at most `k` transactions per unique `StepPath`, at evenly-spaced
    /// indices through that step path's submission order.
    Sample(usize),
    /// Use every watched transaction (CPU-heavy; opt-in via `--benchmark.profile-all`).
    All,
}

/// Sample tx hashes for profiling from the watcher's per-workload submission map.
///
/// The watcher returns an `IndexMap<TxHash, (StepPath, _)>` that preserves
/// submission order. We:
/// 1. Group hashes by `StepPath`, preserving submission order within each group.
/// 2. For `Sample(k)`: pick `k` evenly-spaced indices per group — positions
///    `⌊(N − 1) · i / (k − 1)⌋` for `i ∈ 0..k`. Captures cold (i=0),
///    steady-state (middle), and end-state (i=N−1). If `N ≤ k`, return all.
/// 3. For `All`: return every key.
pub fn sample_watched_txs<V>(
    transactions: &IndexMap<TxHash, (StepPath, V)>,
    mode: SamplingMode,
) -> Vec<TxHash> {
    if let SamplingMode::All = mode {
        return transactions.keys().copied().collect();
    }
    let SamplingMode::Sample(k) = mode else { unreachable!() };
    if k == 0 || transactions.is_empty() {
        return Vec::new();
    }

    let mut by_step: IndexMap<&StepPath, Vec<TxHash>> = IndexMap::new();
    for (tx_hash, (step_path, _)) in transactions {
        by_step.entry(step_path).or_default().push(*tx_hash);
    }

    let mut out = Vec::new();
    for (_, group) in by_step {
        sample_one_group(&group, k, &mut out);
    }
    out
}

fn sample_one_group(group: &[TxHash], k: usize, out: &mut Vec<TxHash>) {
    let n = group.len();
    if n == 0 {
        return;
    }
    if n <= k {
        out.extend_from_slice(group);
        return;
    }
    if k == 1 {
        out.push(group[0]);
        return;
    }
    for i in 0..k {
        let idx = (n - 1) * i / (k - 1);
        out.push(group[idx]);
    }
}

/// Build [`ExecutionTracerConfig`] with our defaults plus a CLI-controlled
/// step cap. Pass `step_limit = 0` to disable the cap.
pub fn execution_tracer_config(step_limit: u64) -> ExecutionTracerConfig {
    ExecutionTracerConfig {
        enable_memory: false,
        disable_stack: true,
        disable_storage: true,
        enable_return_data: false,
        disable_syscall_details: false,
        limit: if step_limit == 0 {
            None
        } else {
            Some(step_limit)
        },
        memory_word_limit: 0,
    }
}

/// One block to trace, plus the `tx_index → (tx_hash, step_path)` map for
/// tagging traces back to samples.
pub struct PerBlockTraceJob {
    pub block_hash: SubxtH256,
    pub watched_indices: HashMap<u32, (TxHash, StepPath)>,
}

/// Resolve sampled hashes to per-block trace jobs. Must be called while the
/// [`TransactionFinder`] is still alive. Samples whose finder lookup yields
/// no substrate block (non-substrate platforms) are silently skipped.
pub async fn build_block_jobs(
    finder: &TransactionFinder,
    samples: Vec<(TxHash, StepPath)>,
) -> Result<Vec<PerBlockTraceJob>> {
    let mut by_block: HashMap<SubxtH256, PerBlockTraceJob> = HashMap::new();

    for (tx_hash, step_path) in samples {
        let (tx_index, block_arc) = finder.find(tx_hash).await;
        let Some(substrate_block) = block_arc.substrate_block.as_ref() else {
            continue;
        };
        let block_hash = substrate_block.hash();

        by_block
            .entry(block_hash)
            .or_insert_with(|| PerBlockTraceJob {
                block_hash,
                watched_indices: HashMap::new(),
            })
            .watched_indices
            .insert(tx_index as u32, (tx_hash, step_path));
    }
    Ok(by_block.into_values().collect())
}

/// Execute trace jobs against a node, aggregating each watched tx to a
/// [`TxProfile`]. Concurrency is bounded by `concurrency`. Per-block failures
/// are logged and skipped — the function returns whatever profiles succeed.
pub async fn execute_trace_jobs(
    node: &dyn NodeApi,
    jobs: Vec<PerBlockTraceJob>,
    step_limit: u64,
    concurrency: usize,
) -> Vec<TxProfile> {
    let concurrency = concurrency.max(1);

    let samples: Vec<(SubxtH256, u32, TxHash, StepPath)> = jobs
        .into_iter()
        .flat_map(|job| {
            let block_hash = job.block_hash;
            job.watched_indices
                .into_iter()
                .map(move |(tx_index, (tx_hash, step_path))| {
                    (block_hash, tx_index, tx_hash, step_path)
                })
        })
        .collect();

    let pending = samples.into_iter().map(|(block_hash, tx_index, tx_hash, step_path)| {
        let trace_future = node.trace_execution_tx(
            block_hash,
            tx_index,
            execution_tracer_config(step_limit),
        );
        async move {
            match trace_future.await {
                Ok(Some(execution_trace)) => Some(TxProfile::from_execution_trace(
                    tx_hash,
                    step_path,
                    &execution_trace,
                )),
                Ok(None) => {
                    warn!(
                        ?block_hash,
                        tx_index,
                        "trace_execution_tx returned None; skipping sample"
                    );
                    None
                }
                Err(err) => {
                    warn!(
                        ?block_hash,
                        tx_index,
                        ?err,
                        "trace_execution_tx failed; skipping sample"
                    );
                    None
                }
            }
        }
    });

    stream::iter(pending)
        .buffer_unordered(concurrency)
        .filter_map(|opt| async move { opt })
        .collect()
        .await
}

/// Maximum number of distinct opcode rows kept in the aggregated rollup.
/// Anything beyond rolls into an "Other" bucket. Keeps report.json bounded
/// (a single trace can have hundreds of distinct PVM syscalls + EVM opcodes).
const OPCODE_TOP_N: usize = 64;

/// Aggregate a workload's `Vec<TxProfile>` into a wire-ready
/// `OpcodeProfileSummary` for the report.
pub fn aggregate_to_summary(
    profiles: Vec<TxProfile>,
    block_count: u32,
) -> OpcodeProfileSummary {
    let sampled_tx_count = profiles.len();
    let failed_count = profiles.iter().filter(|p| p.failed).count();

    let mut by_op: HashMap<String, (u64, u128, u128)> = HashMap::new();
    for profile in &profiles {
        for opcode in &profile.opcodes {
            let key = opcode.op_key.as_string();
            let entry = by_op.entry(key).or_insert((0, 0, 0));
            entry.0 += opcode.count;
            entry.1 += opcode.total_ref_time;
            entry.2 += opcode.total_proof_size;
        }
    }

    let mut sorted: Vec<(String, u64, u128, u128)> = by_op
        .into_iter()
        .map(|(k, (c, rt, ps))| (k, c, rt, ps))
        .collect();
    sorted.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    let mut opcodes: Vec<AggregatedOpcode> = sorted
        .iter()
        .take(OPCODE_TOP_N)
        .map(|(op_key, count, rt, ps)| AggregatedOpcode {
            op_key: op_key.clone(),
            sample_count: *count,
            total_ref_time: *rt,
            total_proof_size: *ps,
        })
        .collect();

    if sorted.len() > OPCODE_TOP_N {
        let (count, rt, ps) = sorted.iter().skip(OPCODE_TOP_N).fold(
            (0u64, 0u128, 0u128),
            |acc, (_, c, rt, ps)| (acc.0 + c, acc.1 + rt, acc.2 + ps),
        );
        opcodes.push(AggregatedOpcode {
            op_key: "Other".to_string(),
            sample_count: count,
            total_ref_time: rt,
            total_proof_size: ps,
        });
    }

    let tx_profiles: Vec<TxProfileWire> = profiles
        .into_iter()
        .map(|p| {
            let opcodes = p
                .opcodes
                .into_iter()
                .map(|o| AggregatedOpcode {
                    op_key: o.op_key.as_string(),
                    sample_count: o.count,
                    total_ref_time: o.total_ref_time,
                    total_proof_size: o.total_proof_size,
                })
                .collect();
            TxProfileWire {
                tx_hash: p.tx_hash,
                step_path: p.step_path,
                failed: p.failed,
                gas_used: p.gas_used,
                weight_consumed_ref_time: p.weight_consumed_ref_time,
                weight_consumed_proof_size: p.weight_consumed_proof_size,
                base_call_weight_ref_time: p.base_call_weight_ref_time,
                base_call_weight_proof_size: p.base_call_weight_proof_size,
                unattributed_ref_time: p.unattributed_ref_time,
                unattributed_proof_size: p.unattributed_proof_size,
                opcodes,
            }
        })
        .collect();

    OpcodeProfileSummary {
        sampled_tx_count,
        block_count,
        failed_count,
        opcodes,
        tx_profiles,
        opcode_catalog: opcode_catalog_wire(),
    }
}

fn opcode_catalog_wire() -> OpcodeCatalogWire {
    let catalog = OpcodeCatalog::current();
    let to_wire = |m: std::collections::BTreeMap<_, _>| {
        m.into_iter()
            .map(|(byte, entry): (u8, revive_dt_node_interaction::opcode_profile::OpcodeEntry)| {
                (
                    byte.to_string(),
                    OpcodeEntryWire {
                        name: entry.name,
                        category: entry.category.to_string(),
                    },
                )
            })
            .collect()
    };
    OpcodeCatalogWire {
        evm: to_wire(catalog.evm),
        pvm: to_wire(catalog.pvm),
        category_order: catalog.category_order.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_hash(tag: u8) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[0] = tag;
        TxHash::from(bytes)
    }

    fn path(indices: &[usize]) -> StepPath {
        StepPath::new(
            indices
                .iter()
                .copied()
                .map(StepIdx::new)
                .collect::<Vec<_>>(),
        )
    }

    fn submissions(items: Vec<(u8, &[usize])>) -> IndexMap<TxHash, (StepPath, ())> {
        let mut m = IndexMap::new();
        for (tag, p) in items {
            m.insert(mk_hash(tag), (path(p), ()));
        }
        m
    }

    #[test]
    fn all_mode_returns_every_key_in_order() {
        let m = submissions(vec![(1, &[0]), (2, &[0]), (3, &[1])]);
        let out = sample_watched_txs(&m, SamplingMode::All);
        assert_eq!(out, vec![mk_hash(1), mk_hash(2), mk_hash(3)]);
    }

    #[test]
    fn k_zero_returns_empty() {
        let m = submissions(vec![(1, &[0]), (2, &[0])]);
        assert!(sample_watched_txs(&m, SamplingMode::Sample(0)).is_empty());
    }

    #[test]
    fn smaller_than_k_returns_all_for_group() {
        // Group has 3 entries, k=5 → return all 3
        let m = submissions(vec![(1, &[0]), (2, &[0]), (3, &[0])]);
        let out = sample_watched_txs(&m, SamplingMode::Sample(5));
        assert_eq!(out, vec![mk_hash(1), mk_hash(2), mk_hash(3)]);
    }

    #[test]
    fn k_one_picks_first_per_group() {
        let m = submissions(vec![
            (1, &[0]),
            (2, &[0]),
            (3, &[0]),
            (4, &[1]),
            (5, &[1]),
        ]);
        let out = sample_watched_txs(&m, SamplingMode::Sample(1));
        assert_eq!(out, vec![mk_hash(1), mk_hash(4)]);
    }

    #[test]
    fn evenly_spaced_indices() {
        // Group of 9 entries (indices 0..=8), k=5
        // Expected positions: ⌊(8 * i) / 4⌋ for i ∈ 0..5 = 0, 2, 4, 6, 8
        let entries: Vec<(u8, &[usize])> = (1u8..=9).map(|i| (i, &[0usize] as &[usize])).collect();
        let m = submissions(entries);
        let out = sample_watched_txs(&m, SamplingMode::Sample(5));
        assert_eq!(
            out,
            vec![
                mk_hash(1), // idx 0
                mk_hash(3), // idx 2
                mk_hash(5), // idx 4
                mk_hash(7), // idx 6
                mk_hash(9), // idx 8
            ]
        );
    }

    #[test]
    fn multiple_groups_each_sampled_independently() {
        // step_path [0]: 5 entries (1..=5), step_path [1]: 5 entries (6..=10)
        // k=3 → positions ⌊(4 * i) / 2⌋ = 0, 2, 4
        let entries: Vec<(u8, &[usize])> = (1..=5)
            .map(|i| (i, &[0usize] as &[usize]))
            .chain((6..=10).map(|i| (i, &[1usize] as &[usize])))
            .collect();
        let m = submissions(entries);
        let out = sample_watched_txs(&m, SamplingMode::Sample(3));
        assert_eq!(
            out,
            vec![
                // step_path [0]
                mk_hash(1), // idx 0
                mk_hash(3), // idx 2
                mk_hash(5), // idx 4
                // step_path [1]
                mk_hash(6),  // idx 0
                mk_hash(8),  // idx 2
                mk_hash(10), // idx 4
            ]
        );
    }

}
