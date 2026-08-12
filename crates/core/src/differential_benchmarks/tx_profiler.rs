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

use revive_dt_node_interaction::opcode_profile;

use crate::internal_prelude::*;

/// The profiling configuration for a single workload's watcher, derived from the
/// benchmark run configuration. The watcher holds this as `Option<ProfilerConfig>`;
/// `None` means profiling is disabled.
#[derive(Debug, Clone, Copy)]
pub struct ProfilerConfig {
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
    step_limit: u64,
    concurrency: usize,
) -> OpcodeProfileSummary {
    let concurrency = concurrency.max(1);
    let selected = samples.len();

    let profiles = stream::iter(samples.into_iter().map(|(tx_hash, step_path)| {
        let trace_future = connector.trace_execution_tx(tx_hash, step_limit);
        async move {
            let trace_future = trace_future?;
            match trace_future.await {
                Ok(Some((block_number, extrinsic_index, execution_trace))) => {
                    Some(opcode_profile::from_execution_trace(
                        tx_hash,
                        step_path,
                        block_number,
                        extrinsic_index,
                        &execution_trace,
                    ))
                }
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
        .map(|profile| profile.block_number)
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

/// Aggregate a workload's `Vec<TxProfile>` into an `OpcodeProfileSummary` for
/// the report: a top-N cross-tx rollup plus the per-tx profiles embedded as-is.
pub fn aggregate_to_summary(profiles: Vec<TxProfile>, block_count: u32) -> OpcodeProfileSummary {
    let sampled_tx_count = profiles.len();
    let failed_count = profiles.iter().filter(|p| p.failed).count();

    #[derive(Default)]
    struct OpcodeTotals {
        count: u64,
        ref_time: u128,
        proof_size: u128,
    }

    let mut by_op = HashMap::<OpKey, OpcodeTotals>::new();
    for profile in &profiles {
        for opcode in &profile.opcodes {
            let totals = by_op.entry(opcode.op).or_default();
            totals.count += opcode.count;
            totals.ref_time += opcode.weight.ref_time() as u128;
            totals.proof_size += opcode.weight.proof_size() as u128;
        }
    }

    let mut sorted = by_op.into_iter().collect::<Vec<_>>();
    sorted.sort_by(|(a_op, a), (b_op, b)| b.ref_time.cmp(&a.ref_time).then_with(|| a_op.cmp(b_op)));

    let mut opcodes = sorted
        .iter()
        .take(OPCODE_TOP_N)
        .map(|(op, totals)| AggregatedOpcode {
            op: AggregatedOpKey::Op(*op),
            count: totals.count,
            total_ref_time: totals.ref_time,
            total_proof_size: totals.proof_size,
        })
        .collect::<Vec<_>>();

    if sorted.len() > OPCODE_TOP_N {
        let other = sorted.iter().skip(OPCODE_TOP_N).fold(
            OpcodeTotals::default(),
            |mut acc, (_, totals)| {
                acc.count += totals.count;
                acc.ref_time += totals.ref_time;
                acc.proof_size += totals.proof_size;
                acc
            },
        );
        opcodes.push(AggregatedOpcode {
            op: AggregatedOpKey::Other,
            count: other.count,
            total_ref_time: other.ref_time,
            total_proof_size: other.proof_size,
        });
    }

    OpcodeProfileSummary {
        sampled_tx_count,
        block_count,
        failed_count,
        opcodes,
        tx_profiles: profiles,
        opcode_catalog: opcode_profile::current_catalog(),
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
        let entries = (1u8..=9)
            .map(|i| (i, &[0usize] as &[usize]))
            .collect::<Vec<_>>();
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
        let entries = (1..=5)
            .map(|i| (i, &[0usize] as &[usize]))
            .chain((6..=10).map(|i| (i, &[1usize] as &[usize])))
            .collect::<Vec<_>>();
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
