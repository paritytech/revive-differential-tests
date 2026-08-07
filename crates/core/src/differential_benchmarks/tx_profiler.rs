//! Watched-tx opcode profiler.
//!
//! At the end of each `(platform, workload)` the watcher samples a handful of
//! transactions per [`StepPath`], re-executes each under the pallet-revive
//! execution tracer via [`NodeConnector::trace_execution_tx`], and aggregates
//! the per-opcode weight breakdown into an [`OpcodeProfileSummary`] for the
//! report.
//!
//! Unlike the pre-`#304` fork, the connector resolves a `tx_hash` to its
//! `(substrate_block, extrinsic_index)` internally, so there's no separate
//! per-block job-building pass — we just drive one trace per sample.

use futures::{StreamExt, stream};
use indexmap::IndexMap;
use revive_dt_node_interaction::opcode_profile::{OpcodeCatalog, OpcodeEntry, TxProfile};
use revive_dt_report::{
    AggregatedOpcode, OpcodeCatalogWire, OpcodeEntryWire, OpcodeProfileSummary, TxProfileWire,
};

use crate::internal_prelude::*;

/// The profiling configuration relevant to a single workload's watcher, derived
/// from the benchmark run configuration.
#[derive(Debug, Clone, Copy)]
pub struct ProfileConfig {
    /// Whether opcode profiling is enabled at all.
    pub enabled: bool,
    /// How watched transactions are chosen for profiling.
    pub mode: SamplingMode,
    /// The tracer step cap. `0` disables the cap.
    pub step_limit: u64,
    /// The number of concurrent traces to run.
    pub concurrency: usize,
}

/// How to choose which watched transactions become profiler input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingMode {
    /// Pick at most `sample_size` transactions per unique `StepPath`, at
    /// evenly-spaced indices through that step path's inclusion order.
    Sample(usize),
    /// Use every watched transaction (CPU-heavy; opt-in via `--benchmark.profile-all`).
    All,
}

/// Sample tx hashes for profiling from an inclusion-ordered per-workload list.
///
/// The caller passes txs in inclusion order (block order, then in-block order),
/// so first/middle/last map to the execution lifecycle. We:
/// 1. Group hashes by `StepPath`, preserving that order within each group.
/// 2. For `Sample(sample_size)`: pick `sample_size` evenly-spaced indices per
///    group — positions `⌊(group_size − 1) · i / (sample_size − 1)⌋` for
///    `i ∈ 0..sample_size`. Captures cold (i=0), steady-state (middle), and
///    end-state (i=group_size−1). If `group_size ≤ sample_size`, return all.
/// 3. For `All`: return every tx unchanged.
pub fn sample_watched_txs(
    txs: &[(TxHash, StepPath)],
    mode: SamplingMode,
) -> Vec<(TxHash, StepPath)> {
    let sample_size = match mode {
        SamplingMode::All => return txs.to_vec(),
        SamplingMode::Sample(sample_size) => sample_size,
    };
    if sample_size == 0 || txs.is_empty() {
        return Vec::new();
    }

    let mut by_step: IndexMap<&StepPath, Vec<TxHash>> = IndexMap::new();
    for (tx_hash, step_path) in txs {
        by_step.entry(step_path).or_default().push(*tx_hash);
    }

    let mut out = Vec::new();
    for (step_path, group) in by_step {
        let mut picks = Vec::new();
        sample_one_group(&group, sample_size, &mut picks);
        out.extend(picks.into_iter().map(|hash| (hash, step_path.clone())));
    }
    out
}

fn sample_one_group(group: &[TxHash], sample_size: usize, out: &mut Vec<TxHash>) {
    let group_size = group.len();
    if group_size == 0 {
        return;
    }
    if group_size <= sample_size {
        out.extend_from_slice(group);
        return;
    }
    if sample_size == 1 {
        out.push(group[0]);
        return;
    }
    for i in 0..sample_size {
        let idx = (group_size - 1) * i / (sample_size - 1);
        out.push(group[idx]);
    }
}

/// Trace each sample and aggregate the successful traces into an [`OpcodeProfileSummary`].
pub async fn run_profiling(
    connector: &NodeConnector,
    samples: Vec<(TxHash, StepPath)>,
    sample_block_numbers: HashMap<TxHash, BlockNumber>,
    step_limit: u64,
    concurrency: usize,
) -> OpcodeProfileSummary {
    let concurrency = concurrency.max(1);
    let selected = samples.len();

    let profiles = stream::iter(samples.into_iter().map(|(tx_hash, step_path)| {
        let trace_future = connector.trace_execution_tx(tx_hash, step_limit);
        async move {
            match trace_future.await {
                Ok(Some(execution_trace)) => Some(TxProfile::from_execution_trace(
                    tx_hash,
                    step_path,
                    &execution_trace,
                )),
                Ok(None) => {
                    warn!(
                        ?tx_hash,
                        "trace_execution_tx returned None; skipping sample"
                    );
                    None
                }
                Err(err) => {
                    warn!(?tx_hash, ?err, "trace_execution_tx failed; skipping sample");
                    None
                }
            }
        }
    }))
    .buffer_unordered(concurrency)
    .filter_map(|opt| async move { opt })
    .collect::<Vec<_>>()
    .await;

    let block_count = profiles
        .iter()
        .filter_map(|profile| sample_block_numbers.get(&profile.tx_hash))
        .collect::<HashSet<_>>()
        .len() as u32;
    if profiles.len() < selected {
        warn!(
            selected,
            traced = profiles.len(),
            block_count,
            "Some sampled txs failed to trace; the profile covers fewer txs than were selected"
        );
    } else {
        info!(
            selected,
            traced = profiles.len(),
            block_count,
            "Profiling traces complete"
        );
    }

    aggregate_to_summary(profiles, block_count)
}

/// Max opcode rows in the `summary` rollup; the rest roll into "Other". Caps only
/// the summary; per-tx `tx_profiles` keep their full opcode lists.
const OPCODE_TOP_N: usize = 64;

/// Aggregate a workload's `Vec<TxProfile>` into a wire-ready
/// `OpcodeProfileSummary` for the report.
pub fn aggregate_to_summary(profiles: Vec<TxProfile>, block_count: u32) -> OpcodeProfileSummary {
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
        let (count, rt, ps) = sorted
            .iter()
            .skip(OPCODE_TOP_N)
            .fold((0u64, 0u128, 0u128), |acc, (_, c, rt, ps)| {
                (acc.0 + c, acc.1 + rt, acc.2 + ps)
            });
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
    let to_wire = |m: std::collections::BTreeMap<u8, OpcodeEntry>| {
        m.into_iter()
            .map(|(byte, entry)| {
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
        category_order: catalog
            .category_order
            .iter()
            .map(|s| s.to_string())
            .collect(),
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

    fn submissions(items: Vec<(u8, &[usize])>) -> Vec<(TxHash, StepPath)> {
        items
            .into_iter()
            .map(|(tag, p)| (mk_hash(tag), path(p)))
            .collect()
    }

    // Run the sampler and drop the step path for hash-only assertions.
    fn sampled_hashes(txs: &[(TxHash, StepPath)], mode: SamplingMode) -> Vec<TxHash> {
        sample_watched_txs(txs, mode)
            .into_iter()
            .map(|(hash, _)| hash)
            .collect()
    }

    #[test]
    fn all_mode_returns_every_key_in_order() {
        let m = submissions(vec![(1, &[0]), (2, &[0]), (3, &[1])]);
        let out = sampled_hashes(&m, SamplingMode::All);
        assert_eq!(out, vec![mk_hash(1), mk_hash(2), mk_hash(3)]);
    }

    #[test]
    fn k_zero_returns_empty() {
        let m = submissions(vec![(1, &[0]), (2, &[0])]);
        assert!(sampled_hashes(&m, SamplingMode::Sample(0)).is_empty());
    }

    #[test]
    fn smaller_than_k_returns_all_for_group() {
        // Group has 3 entries, sample_size=5 → return all 3
        let m = submissions(vec![(1, &[0]), (2, &[0]), (3, &[0])]);
        let out = sampled_hashes(&m, SamplingMode::Sample(5));
        assert_eq!(out, vec![mk_hash(1), mk_hash(2), mk_hash(3)]);
    }

    #[test]
    fn k_one_picks_first_per_group() {
        let m = submissions(vec![(1, &[0]), (2, &[0]), (3, &[0]), (4, &[1]), (5, &[1])]);
        let out = sampled_hashes(&m, SamplingMode::Sample(1));
        assert_eq!(out, vec![mk_hash(1), mk_hash(4)]);
    }

    #[test]
    fn evenly_spaced_indices() {
        // Group of 9 entries (indices 0..=8), sample_size=5
        // Expected positions: ⌊(8 * i) / 4⌋ for i ∈ 0..5 = 0, 2, 4, 6, 8
        let entries: Vec<(u8, &[usize])> = (1u8..=9).map(|i| (i, &[0usize] as &[usize])).collect();
        let m = submissions(entries);
        let out = sampled_hashes(&m, SamplingMode::Sample(5));
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
        // sample_size=3 → positions ⌊(4 * i) / 2⌋ = 0, 2, 4
        let entries: Vec<(u8, &[usize])> = (1..=5)
            .map(|i| (i, &[0usize] as &[usize]))
            .chain((6..=10).map(|i| (i, &[1usize] as &[usize])))
            .collect();
        let m = submissions(entries);
        let out = sampled_hashes(&m, SamplingMode::Sample(3));
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
