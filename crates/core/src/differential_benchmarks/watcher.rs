use std::{collections::HashSet, pin::Pin, sync::Arc};

use alloy::primitives::{BlockNumber, TxHash};
use anyhow::Result;
use futures::{Stream, StreamExt};
use revive_dt_common::types::PlatformIdentifier;
use revive_dt_node_interaction::MinedBlockInformation;
use tokio::sync::{
    RwLock,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tracing::{info, instrument};

/// This struct defines the watcher used in the benchmarks. A watcher is only valid for 1 workload
/// and MUST NOT be re-used between workloads since it holds important internal state for a given
/// workload and is not designed for reuse.
pub struct Watcher {
    /// The identifier of the platform that this watcher is for.
    platform_identifier: PlatformIdentifier,

    /// The receive side of the channel that all of the drivers and various other parts of the code
    /// send events to the watcher on.
    rx: UnboundedReceiver<WatcherEvent>,

    /// This is a stream of the blocks that were mined by the node. This is for a single platform
    /// and a single node from that platform.
    blocks_stream: Pin<Box<dyn Stream<Item = MinedBlockInformation>>>,
}

impl Watcher {
    pub fn new(
        platform_identifier: PlatformIdentifier,
        blocks_stream: Pin<Box<dyn Stream<Item = MinedBlockInformation>>>,
    ) -> (Self, UnboundedSender<WatcherEvent>) {
        let (tx, rx) = unbounded_channel::<WatcherEvent>();
        (
            Self {
                platform_identifier,
                rx,
                blocks_stream,
            },
            tx,
        )
    }

    #[instrument(level = "info", skip_all)]
    pub async fn run(mut self) -> Result<()> {
        // The first event that the watcher receives must be a `RepetitionStartEvent` that informs
        // the watcher of the last block number that it should ignore and what the block number is
        // for the first important block that it should look for.
        let ignore_block_before = loop {
            let Some(WatcherEvent::RepetitionStartEvent {
                ignore_block_before,
            }) = self.rx.recv().await
            else {
                continue;
            };
            break ignore_block_before;
        };

        // This is the set of the transaction hashes that the watcher should be looking for and
        // watch for them in the blocks. The watcher will keep watching for blocks until it sees
        // that all of the transactions that it was watching for has been seen in the mined blocks.
        let watch_for_transaction_hashes = Arc::new(RwLock::new(HashSet::<TxHash>::new()));

        // A boolean that keeps track of whether all of the transactions were submitted or if more
        // txs are expected to come through the receive side of the channel. We do not want to rely
        // on the channel closing alone for the watcher to know that all of the transactions were
        // submitted and for there to be an explicit event sent by the core orchestrator that
        // informs the watcher that no further transactions are to be expected and that it can
        // safely ignore the channel.
        let all_transactions_submitted = Arc::new(RwLock::new(false));

        let watcher_event_watching_task = {
            let watch_for_transaction_hashes = watch_for_transaction_hashes.clone();
            let all_transactions_submitted = all_transactions_submitted.clone();
            async move {
                while let Some(watcher_event) = self.rx.recv().await {
                    match watcher_event {
                        // Subsequent repetition starts are ignored since certain workloads can
                        // contain nested repetitions and therefore there's no use in doing any
                        // action if the repetitions are nested.
                        WatcherEvent::RepetitionStartEvent { .. } => {}
                        WatcherEvent::SubmittedTransaction { transaction_hash } => {
                            watch_for_transaction_hashes
                                .write()
                                .await
                                .insert(transaction_hash);
                        }
                        WatcherEvent::AllTransactionsSubmitted => {
                            *all_transactions_submitted.write().await = true;
                            self.rx.close();
                            info!("Watcher's Events Watching Task Finished");
                            break;
                        }
                    }
                }
            }
        };
        let block_information_watching_task = {
            let watch_for_transaction_hashes = watch_for_transaction_hashes.clone();
            let all_transactions_submitted = all_transactions_submitted.clone();
            let mut blocks_information_stream = self.blocks_stream;
            async move {
                let mut mined_blocks_information = Vec::new();

                // region:TEMPORARY
                eprintln!("Watcher information for {}", self.platform_identifier);
                eprintln!(
                    "block_number,block_timestamp,mined_gas,block_gas_limit,tx_count,ref_time,max_ref_time,proof_size,max_proof_size"
                );
                // endregion:TEMPORARY
                while let Some(block) = blocks_information_stream.next().await {
                    // If the block number is equal to or less than the last block before the
                    // repetition then we ignore it and continue on to the next block.
                    if block.ethereum_block_information.block_number <= ignore_block_before {
                        continue;
                    }

                    if *all_transactions_submitted.read().await
                        && watch_for_transaction_hashes.read().await.is_empty()
                    {
                        break;
                    }

                    info!(
                        block_number = block.ethereum_block_information.block_number,
                        block_tx_count = block.ethereum_block_information.transaction_hashes.len(),
                        remaining_transactions = watch_for_transaction_hashes.read().await.len(),
                        "Observed a block"
                    );

                    // Remove all of the transaction hashes observed in this block from the txs we
                    // are currently watching for.
                    let mut watch_for_transaction_hashes =
                        watch_for_transaction_hashes.write().await;
                    for tx_hash in block.ethereum_block_information.transaction_hashes.iter() {
                        watch_for_transaction_hashes.remove(tx_hash);
                    }

                    // region:TEMPORARY
                    // TODO: The following core is TEMPORARY and will be removed once we have proper
                    // reporting in place and then it can be removed. This serves as as way of doing
                    // some very simple reporting for the time being.
                    eprintln!(
                        "\"{}\",\"{}\",\"{}\",\"{}\",\"{}\",\"{:?}\",\"{:?}\",\"{:?}\",\"{:?}\"",
                        block.ethereum_block_information.block_number,
                        block.ethereum_block_information.block_timestamp,
                        block.ethereum_block_information.mined_gas,
                        block.ethereum_block_information.block_gas_limit,
                        block.ethereum_block_information.transaction_hashes.len(),
                        block
                            .substrate_block_information
                            .as_ref()
                            .map(|block| block.ref_time),
                        block
                            .substrate_block_information
                            .as_ref()
                            .map(|block| block.max_ref_time),
                        block
                            .substrate_block_information
                            .as_ref()
                            .map(|block| block.proof_size),
                        block
                            .substrate_block_information
                            .as_ref()
                            .map(|block| block.max_proof_size),
                    );
                    // endregion:TEMPORARY

                    mined_blocks_information.push(block);
                }

                info!("Watcher's Block Watching Task Finished");
                mined_blocks_information
            }
        };

        let (_, _) =
            futures::future::join(watcher_event_watching_task, block_information_watching_task)
                .await;

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WatcherEvent {
    /// Informs the watcher that it should begin watching for the blocks mined by the platforms.
    /// Before the watcher receives this event it will not be watching for the mined blocks. The
    /// reason behind this is that we do not want the initialization transactions (e.g., contract
    /// deployments) to be included in the overall TPS and GPS measurements since these blocks will
    /// most likely only contain a single transaction since they're just being used for
    /// initialization.
    RepetitionStartEvent {
        /// This is the block number of the last block seen before the repetition started. This is
        /// used to instruct the watcher to ignore all block prior to this block when it starts
        /// streaming the blocks.
        ignore_block_before: BlockNumber,
    },

    /// Informs the watcher that a transaction was submitted and that the watcher should watch for a
    /// transaction with this hash in the blocks that it watches.
    SubmittedTransaction {
        /// The hash of the submitted transaction.
        transaction_hash: TxHash,
    },

    /// Informs the watcher that all of the transactions of this benchmark have been submitted and
    /// that it can expect to receive no further transaction hashes and not even watch the channel
    /// any longer.
    AllTransactionsSubmitted,
}
