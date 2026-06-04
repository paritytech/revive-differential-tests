use crate::internal_prelude::*;
use pallet_revive::Weight;

/// This struct defines the watcher used in the benchmarks. A watcher is only valid for 1 workload
/// and MUST NOT be re-used between workloads since it holds important internal state for a given
/// workload and is not designed for reuse.
pub struct Watcher {
    /// The receive side of the channel that all of the drivers and various other parts of the code
    /// send events to the watcher on.
    rx: UnboundedReceiver<WatcherEvent>,

    /// The primary connection provider for the network.
    connector: Arc<NodeConnector>,

    /// The reporter used to send events to the report aggregator.
    reporter: ExecutionSpecificReporter,
}

impl Watcher {
    pub fn new(
        connector: Arc<NodeConnector>,
        reporter: ExecutionSpecificReporter,
    ) -> (Self, UnboundedSender<WatcherEvent>) {
        let (tx, rx) = unbounded_channel::<WatcherEvent>();
        (
            Self {
                rx,
                connector,
                reporter,
            },
            tx,
        )
    }

    pub fn run(self) -> StaticFuture<Result<()>> {
        Box::pin(async move {
            let Self {
                mut rx,
                connector,
                reporter,
            } = self;
            let ignore_blocks_before_block_number = loop {
                let Some(WatcherEvent::StartEvent {
                    ignore_block_before,
                }) = rx.recv().await
                else {
                    continue;
                };
                break ignore_block_before;
            };

            let is_submission_completed = Arc::new(RwLock::new(false));
            let txs_to_watch_for = Arc::new(RwLock::new(HashSet::<TxHash>::new()));

            let watcher_event_consumer_task = {
                let is_submission_completed = is_submission_completed.clone();
                let txs_to_watch_for = txs_to_watch_for.clone();

                async move {
                    let mut transaction_information = IndexMap::new();
                    while let Some(event) = rx.recv().await {
                        match event {
                            WatcherEvent::StartEvent { .. } => {}
                            WatcherEvent::SubmittedTransaction {
                                transaction_hash,
                                step_path,
                                submission_time,
                            } => {
                                txs_to_watch_for.write().await.insert(transaction_hash);
                                transaction_information.insert(
                                    transaction_hash,
                                    SubmittedTransactionInformation {
                                        step_path,
                                        submitted_at: submission_time,
                                    },
                                );
                            }
                            WatcherEvent::AllTransactionsSubmitted => {
                                info!("All transactions submitted");
                                *is_submission_completed.write().await = true;
                                break;
                            }
                        }
                    }
                    transaction_information
                }
            };

            let block_watching_task = {
                let is_submission_completed = is_submission_completed.clone();
                let txs_to_watch_for = txs_to_watch_for.clone();
                let mut block_subscription = connector.subscribe_to_finalized_blocks();

                async move {
                    let mut number_of_consecutive_blocks_with_zero_transactions = 0usize;

                    let mut observed_transaction_hashes = HashSet::new();
                    let mut observed_blocks = vec![];

                    while let Some(block) = block_subscription.next().await {
                        let block_number = block.evm_block.number();

                        if block_number < ignore_blocks_before_block_number {
                            info!(
                                block_number,
                                ignore_blocks_before_block_number,
                                "Observed a block, but it's being ignored"
                            );
                            continue;
                        }

                        observed_transaction_hashes.extend(block.evm_block.transactions.hashes());
                        let requested_transactions_len = txs_to_watch_for.read().await.len();
                        info!(
                            block.number = block.evm_block.number(),
                            block.timestamp = block.evm_block.header.timestamp,
                            block.observation_time = block
                                .observation_time
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis(),
                            block.tx_hashes_len = block.evm_block.transactions.len(),
                            transactions.observed = observed_transaction_hashes.len(),
                            transactions.requested = requested_transactions_len,
                            transactions.remaining = requested_transactions_len
                                .saturating_sub(observed_transaction_hashes.len()),
                            "Observed a new block"
                        );

                        if block.evm_block.transactions.is_empty() {
                            number_of_consecutive_blocks_with_zero_transactions += 1;
                        } else {
                            number_of_consecutive_blocks_with_zero_transactions = 0
                        };
                        if number_of_consecutive_blocks_with_zero_transactions >= 30 {
                            bail!("30 blocks have went by with 0 transaction in them. Stopping");
                        }

                        let block_information = ObservedBlockInformation {
                            pending_transactions_when_observed: requested_transactions_len
                                .saturating_sub(observed_transaction_hashes.len()),
                            block,
                        };
                        observed_blocks.push(block_information);

                        if *is_submission_completed.read().await
                            && txs_to_watch_for
                                .read()
                                .await
                                .is_subset(&observed_transaction_hashes)
                        {
                            info!("All transactions observed");
                            break;
                        }
                    }
                    Ok(observed_blocks)
                }
            };

            // When resolved, this is the point when:
            // 1. All txs have been submitted.
            // 2. All txs have been included in finalized blocks.
            let (tx_information, observed_blocks) = try_join(
                tokio::spawn(watcher_event_consumer_task),
                tokio::spawn(block_watching_task),
            )
            .await
            .context("One of the watcher tasks failed")?;
            let observed_blocks = observed_blocks.context("Error observing the blocks")?;

            let receipts = stream::iter(tx_information.keys().copied())
                .map(|tx_hash| connector.get_receipt(tx_hash))
                .buffered(100)
                .try_collect::<Vec<_>>()
                .await
                .context("Failed to get the receipt of some transactions")?;
            let failing_receipt_count = receipts
                .iter()
                .filter(|receipt| !receipt.status())
                .inspect(|receipt| error!(?receipt, "Encountered a failing receipt"))
                .count();
            if failing_receipt_count != 0 {
                bail!("Encountered failing receipts when watching")
            }

            for block_info in observed_blocks {
                for hash in block_info.block.evm_block.transactions.hashes() {
                    let Some(info) = tx_information.get(&hash) else {
                        continue;
                    };
                    reporter
                        .report_step_transaction_information_event(
                            info.step_path.clone(),
                            TransactionInformation {
                                transaction_hash: hash,
                                submission_timestamp: info
                                    .submitted_at
                                    .duration_since(UNIX_EPOCH)
                                    .expect("OS Time is misconfigured")
                                    .as_secs(),
                                block_timestamp: block_info.block.evm_block.header.timestamp,
                                block_number: block_info.block.evm_block.number(),
                            },
                        )
                        .expect("qed; can't fail");
                }

                let mut mined_block_information = MinedBlockInformation {
                    ethereum_block_information: EthereumMinedBlockInformation {
                        block_number: block_info.block.evm_block.number(),
                        block_timestamp: block_info.block.evm_block.header.timestamp,
                        mined_gas: block_info.block.evm_block.header.gas_used as _,
                        block_gas_limit: block_info.block.evm_block.header.gas_limit as _,
                        transaction_hashes: block_info
                            .block
                            .evm_block
                            .transactions
                            .hashes()
                            .collect(),
                    },
                    substrate_block_information: None,
                    tx_counts: block_info.block.evm_block.transactions.hashes().fold(
                        BTreeMap::new(),
                        |mut acc, tx_hash| match tx_information.get(&tx_hash) {
                            Some(info) => {
                                *acc.entry(info.step_path.clone()).or_default() += 1;
                                acc
                            }
                            None => acc,
                        },
                    ),
                    observation_time: block_info.block.observation_time,
                    pending_transaction_count: block_info.pending_transactions_when_observed,
                };

                if let Some(substrate_block) = block_info.block.substrate_block.as_ref() {
                    let pre_dispatch_weights =
                        stream::iter(substrate_block.eth_transactions.iter())
                            .map(|value| value.payload.clone())
                            .then(|payload| {
                                connector.pre_dispatch_weights(substrate_block.block_hash, payload)
                            })
                            .try_collect::<Vec<_>>()
                            .await;
                    let block_pre_dispatch_weight = match pre_dispatch_weights {
                        Ok(weights) => weights
                            .into_iter()
                            .fold(Weight::zero(), Weight::saturating_add),
                        Err(_) => Weight::MAX,
                    };

                    mined_block_information.substrate_block_information =
                        Some(SubstrateMinedBlockInformation {
                            ref_time: substrate_block.consumed_weight.ref_time() as _,
                            max_ref_time: substrate_block.limits.ref_time() as _,
                            proof_size: substrate_block.consumed_weight.proof_size() as _,
                            max_proof_size: substrate_block.limits.proof_size() as _,
                            block_hash: substrate_block.block_hash,
                            pre_dispatch_ref_time: block_pre_dispatch_weight.ref_time() as _,
                            pre_dispatch_proof_size: block_pre_dispatch_weight.proof_size() as _,
                        });
                }
                reporter
                    .report_block_mined_event(mined_block_information)
                    .expect("qed; can't fail")
            }

            Ok(())
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WatcherEvent {
    /// Informs the watcher that it should begin watching for the blocks mined by the platforms.
    /// Before the watcher receives this event it will not be watching for the mined blocks. The
    /// reason behind this is that we do not want the initialization transactions (e.g., contract
    /// deployments) to be included in the overall TPS and GPS measurements since these blocks will
    /// most likely only contain a single transaction since they're just being used for
    /// initialization.
    StartEvent {
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
        /// The step path of the step that the transaction belongs to.
        step_path: StepPath,
        /// The time when the transaction was submitted.
        ///
        /// This field is included on the event itself as opposed to being handled by the receiver
        /// of the event in order to ensure that we don't get inaccurate timestamps if the receiver
        /// ends up lagging behind for one reason or another.
        submission_time: SystemTime,
    },
    /// Informs the watcher that all of the transactions of this benchmark have been submitted and
    /// that it can expect to receive no further transaction hashes and not even watch the channel
    /// any longer.
    AllTransactionsSubmitted,
}

struct SubmittedTransactionInformation {
    pub step_path: StepPath,
    pub submitted_at: SystemTime,
}

struct ObservedBlockInformation {
    pub block: BlockPair,
    pub pending_transactions_when_observed: usize,
}
