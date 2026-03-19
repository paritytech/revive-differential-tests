use crate::internal_prelude::*;

/// This struct defines the watcher used in the benchmarks. A watcher is only valid for 1 workload
/// and MUST NOT be re-used between workloads since it holds important internal state for a given
/// workload and is not designed for reuse.
pub struct Watcher {
    /// The receive side of the channel that all of the drivers and various other parts of the code
    /// send events to the watcher on.
    rx: UnboundedReceiver<WatcherEvent>,

    /// This is a stream of the blocks that were mined by the node. This is for a single platform
    /// and a single node from that platform.
    blocks_stream: FrameworkStream<MinedBlockInformation>,

    /// This is the provider to use to communicate with the eth-rpc. We use this to get the receipts
    /// of the various blocks that get mined for ethereum native chains.
    provider: DynProvider,

    /// This is the substrate RPC which we use to communicate with substrate based nodes. We use
    /// this provider only for chains which are substrate based.
    substrate_provider: Option<OnlineClient<PolkadotConfig>>,

    /// The reporter used to send events to the report aggregator.
    reporter: ExecutionSpecificReporter,
}

impl Watcher {
    pub fn new(
        blocks_stream: FrameworkStream<MinedBlockInformation>,
        provider: DynProvider,
        substrate_provider: Option<OnlineClient<PolkadotConfig>>,
        reporter: ExecutionSpecificReporter,
    ) -> (Self, UnboundedSender<WatcherEvent>) {
        let (tx, rx) = unbounded_channel::<WatcherEvent>();
        (
            Self {
                rx,
                blocks_stream,
                provider,
                substrate_provider,
                reporter,
            },
            tx,
        )
    }

    /// The main future which should be polled in order for the watcher to run.
    ///
    /// This task spawns three concurrent tasks which work together until a final outcome is reached.
    ///
    /// 1. **Registration task**: Listens for [`WatcherEvent`]s. For each transaction that it
    ///    encounters it adds its hash to a `watching_transaction_hashes` [`HashSet`] which means
    ///    that the task is watching for this transaction hash and waiting for it to be mined into a
    ///    block. This is akin to a user sending a request saying "please watch for this transaction
    ///    hash for me" and this task is the handler of such a request. All that it does is that it
    ///    stores the hash of this transaction in a shared [`HashSet`] for the other tasks to handle
    ///    later on. Additionally, when this task receives a
    ///    [`WatcherEvent::AllTransactionsSubmitted`] event it mutates a shared boolean which informs
    ///    the other tasks of the fact that submission is completed. It also guarantees that no more
    ///    transaction hashes will be requested to be watched for by closing the channel completely.
    /// 2. **Block subscription task**: Watches for blocks getting mined and notes down all of the
    ///    transactions that it has observed in these blocks. It does so by storing them in a
    ///    [`HashSet`] of transaction hashes. Eventually, when the shared boolean denoting the
    ///    completion of submission is set to [`true`], this task compares its set of observed
    ///    transactions to the set of transactions that we wanted to watch for. If the set has no
    ///    differences it means that we've successfully observed all of the transactions we wanted
    ///    to observe and this task completes.
    /// 3. **Receipt watching task**: Consumes the `receipts_stream` provided at construction time,
    ///    collecting all transaction receipts as they arrive from the finalized blocks. Like the
    ///    block subscription task, it completes once all requested transaction hashes have had their
    ///    receipts observed.
    ///
    /// All three tasks run concurrently via [`join3`]. Once they all resolve, the watcher checks
    /// that all collected receipts have a successful status. If any receipt indicates a failed
    /// transaction, this future resolves with an [`Err`].
    ///
    /// Finally, the watcher reports all observed block information, per-block transaction counts,
    /// and per-transaction metadata (submission time, block inclusion time) to the reporter.
    #[allow(irrefutable_let_patterns)]
    pub fn run(mut self) -> FrameworkFuture<Result<()>> {
        Box::pin(async move {
            // We start by waiting for the `StartEvent` which informs us of the first block that we want
            // to watch for. Any event that we receive before this `StartEvent` is ignored as the driver
            // is allowed to send events to the watcher before watching has started.
            let ignore_blocks_before_block_number = loop {
                let Some(WatcherEvent::StartEvent {
                    ignore_block_before,
                }) = self.rx.recv().await
                else {
                    continue;
                };
                break ignore_block_before;
            };

            /* Initializing the shared state between the two tasks we will be spawning */

            // This boolean is shared between the two tasks and it's a boolean which the registration
            // task uses to denote the block subscription task when the submission of transactions has
            // completed and no more transactions will be submitted. When this boolean is set to `true`
            // it means that not a single hash more will be added to the set of transactions we're
            // watching for making the set immutable at that point.
            let is_submission_completed = Arc::new(RwLock::new(false));

            // This is the set of transaction hashes which the registration task were asked to register
            // and have the block subscription task watch for. This is the absolute and canonical set of
            // transaction hashes we expect to see mined in the blocks and any other transaction hashes
            // which are observed in blocks are accidental or non canonical.
            let watch_requests = Arc::new(RwLock::new(HashSet::<TxHash>::new()));

            // The registration task, which performs what's been described in the doc-comment of this
            // method.
            let registration_task = tokio::spawn({
                let is_submission_completed = is_submission_completed.clone();
                let watch_requests = watch_requests.clone();
                async move {
                    let mut transaction_information = IndexMap::new();
                    while let Some(watcher_event) = self.rx.recv().await {
                        match watcher_event {
                            // A start event which was resent to the watcher. There's nothing that we do
                            // about this event since the drivers are permitted to resend this event
                            // even after the initial start had happened and therefore we skip this
                            // event without doing any kind of processing on it.
                            WatcherEvent::StartEvent { .. } => {}
                            // The driver submitted a transaction to the chain and therefore it's being
                            // registered for the block subscription task to watch for it. We add it to
                            // the shared state between us and the subscription task and also to the map
                            // that will be returned when this task resolves containing the step that
                            // the transaction belongs to as well as when it was submitted.
                            WatcherEvent::SubmittedTransaction {
                                transaction_hash,
                                step_path,
                                submission_time,
                            } => {
                                transaction_information
                                    .insert(transaction_hash, (step_path, submission_time));
                                watch_requests.write().await.insert(transaction_hash);
                            }
                            // The driver has completed the submission of all of the tasks at this point
                            // and we can safely stop this task. We update the `is_submission_completed`
                            // shared boolean and then we break out of the loop.
                            WatcherEvent::AllTransactionsSubmitted => {
                                info!(
                                    tx_count = transaction_information.len(),
                                    "All transactions have been submitted"
                                );
                                self.rx.close();
                                *is_submission_completed.write().await = true;
                                break;
                            }
                        }
                    }
                    transaction_information
                }
            });

            // This is the block subscription task which is the main watcher task watching for all of
            // the observed transactions and keeping track of them. It's implemented as described in the
            // doc comment of the function.
            let block_subscription_task = tokio::spawn({
                let is_submission_completed = is_submission_completed.clone();
                let watch_requests = watch_requests.clone();
                async move {
                    // This is a kill switch which we use in cases where it appears like the chain
                    // has stopped processing transactions and appears to have halted. If we have
                    // encountered 100 blocks with no transactions then we stop this task and move
                    // on even if we are yet to observe all of the transactions. We also provide a
                    // warning that this was the halting reason just in case it needs further
                    // investigation.
                    let mut number_of_consecutive_blocks_with_zero_transactions = 0usize;

                    let mut observed_transaction_hashes = HashSet::new();
                    let mut observed_blocks = vec![];
                    while let Some(block_information) = self.blocks_stream.next().await {
                        let block_number =
                            block_information.ethereum_block_information.block_number;

                        // Keep skipping block as long as their block number is below the block number
                        // we were tasked to start at.
                        if block_number < ignore_blocks_before_block_number {
                            info!(
                                block_number =
                                    block_information.ethereum_block_information.block_number,
                                ignore_blocks_before_block_number,
                                "Observed a block, but it's being ignored"
                            );
                            continue;
                        }

                        // Add the transaction hashes from this block to the set of transaction hashes
                        // we've observed and also add it's information to the map we store where we
                        // keep track of the transaction and which block contained it.
                        observed_transaction_hashes.extend(
                            block_information
                                .ethereum_block_information
                                .transaction_hashes
                                .clone(),
                        );

                        // Logging information about this newly observed block and adding it to the set
                        // of block we will return at the end.
                        let requested_transactions_len = watch_requests.read().await.len();
                        info!(
                            block.number =
                                block_information.ethereum_block_information.block_number,
                            block.timestamp =
                                block_information.ethereum_block_information.block_timestamp,
                            block.observation_time = block_information
                                .observation_time
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis(),
                            block.tx_hashes_len = block_information
                                .ethereum_block_information
                                .transaction_hashes
                                .len(),
                            transactions.observed = observed_transaction_hashes.len(),
                            transactions.requested = requested_transactions_len,
                            transactions.remaining = requested_transactions_len
                                .saturating_sub(observed_transaction_hashes.len()),
                            "Observed a new block"
                        );
                        let has_transactions = !block_information
                            .ethereum_block_information
                            .transaction_hashes
                            .is_empty();
                        observed_blocks.push(block_information);

                        if !has_transactions {
                            number_of_consecutive_blocks_with_zero_transactions += 1
                        } else {
                            number_of_consecutive_blocks_with_zero_transactions = 0
                        }

                        // This is the primary condition which determines if we should break out of the
                        // loop or not. If all of the transactions have been submitted and there's no
                        // difference between the transactions the user requested us to watch and the
                        // ones we've seen then we break out of the loop and stop.
                        let all_blocks_observed = watch_requests
                            .read()
                            .await
                            .is_subset(&observed_transaction_hashes);
                        let block_empty_timeout =
                            number_of_consecutive_blocks_with_zero_transactions >= 100;
                        if *is_submission_completed.read().await
                            && (all_blocks_observed || block_empty_timeout)
                        {
                            info!(
                                all_blocks_observed,
                                block_empty_timeout, "All transactions observed"
                            );
                            break;
                        }
                    }
                    observed_blocks
                }
            });

            // Execute both of the tasks concurrently until they both complete.
            let (transaction_registration_information, observed_blocks) =
                try_join(registration_task, block_subscription_task).await?;

            // The registration and the block observation tasks are done. We can now try to get the
            // receipts of all of the transactions that were submitted. We will get them through the
            // rpc depending on the platform. The range of receipts we get is the ones for all of
            // the blocks which we've observed.
            let observed_receipts: Vec<TransactionReceipt> = match self.substrate_provider {
                Some(substrate_provider) => {
                    let receipt_extractor = ReceiptExtractor::new(substrate_provider.clone(), None)
                        .await
                        .context("Failed to create the receipt extractor")
                        .map(Arc::new)?;
                    let block_data: Vec<_> = observed_blocks
                        .iter()
                        .map(|b| {
                            let block_hash = b
                                .substrate_block_information
                                .as_ref()
                                .expect("Substrate block information must be present")
                                .block_hash;
                            (block_hash, b.ethereum_block_information.block_number)
                        })
                        .collect();
                    stream::iter(block_data)
                        .map(|(block_hash, block_number)| {
                            let receipt_extractor = receipt_extractor.clone();
                            let substrate_provider = substrate_provider.clone();
                            async move {
                                let block =
                                    retry_with_exponential_backoff(10, Duration::from_secs(1), || async {
                                        match substrate_provider.blocks().at(H256(block_hash)).await {
                                            Ok(block) => ControlFlow::Break(block),
                                            Err(err) => {
                                                warn!(
                                                    block_number,
                                                    ?err,
                                                    "Failed to get the block, retrying"
                                                );
                                                ControlFlow::Continue(err)
                                            }
                                        }
                                    })
                                    .await
                                    .with_context(|| {
                                        format!("Failed to get block {block_number} after retries")
                                    })
                                    .expect("Can't fail");

                                let receipts =
                                    retry_with_exponential_backoff(10, Duration::from_secs(1), || async {
                                        match receipt_extractor.extract_from_block(&block).await {
                                            Ok(receipts) => ControlFlow::Break(receipts),
                                            Err(err) => {
                                                warn!(
                                                    block_number,
                                                    ?err,
                                                    "Failed to get the receipts for block, retrying"
                                                );
                                                ControlFlow::Continue(err)
                                            }
                                        }
                                    })
                                    .await
                                    .with_context(|| {
                                        format!(
                                            "Failed to get receipts for block {block_number} after retries"
                                        )
                                    })
                                    .expect("Can't fail");

                                receipts
                                    .into_iter()
                                    .map(|(_, receipt)| {
                                        let serialized = serde_json::to_vec(&receipt).expect("Can't fail");
                                        serde_json::from_slice::<TransactionReceipt>(&serialized)
                                            .expect("Can't fail")
                                    })
                                    .collect::<Vec<_>>()
                            }
                        })
                        .buffer_unordered(100)
                        .flat_map(|receipts| stream::iter(receipts.into_iter()))
                        .collect::<Vec<_>>()
                        .await
                }
                None => {
                    let block_numbers: Vec<_> = observed_blocks
                        .iter()
                        .map(|b| b.ethereum_block_information.block_number)
                        .collect();
                    stream::iter(block_numbers)
                        .map(|block_number| {
                            let provider = self.provider.clone();
                            async move {
                                retry_with_exponential_backoff(10, Duration::from_secs(1), || async {
                                    match provider
                                        .get_block_receipts(alloy::eips::BlockId::Number(block_number.into()))
                                        .await
                                    {
                                        Ok(Some(receipts)) => ControlFlow::Break(receipts),
                                        other => {
                                            warn!(
                                                block_number,
                                                result = ?other,
                                                "Failed to get the receipts for block, retrying"
                                            );
                                            ControlFlow::Continue(anyhow::anyhow!("{other:?}"))
                                        }
                                    }
                                })
                                .await
                                .with_context(|| {
                                    format!("Failed to get receipts for block {block_number} after retries")
                                })
                                .expect("Can't fail")
                            }
                        })
                        .buffer_unordered(100)
                        .flat_map(|receipts| stream::iter(receipts.into_iter()))
                        .collect::<Vec<_>>()
                        .await
                }
            };

            if let failing_receipts = observed_receipts
                .into_iter()
                .filter(|receipt| !receipt.status())
                .collect::<Vec<_>>()
                && !failing_receipts.is_empty()
            {
                bail!(
                    "Encountered failing receipts of {} len: {failing_receipts:?}",
                    failing_receipts.len()
                );
            }

            // Reporting all of the information to the reporter about all of the observed blocks, the tx
            // counts, the transaction information, and everything else.
            for mut block in observed_blocks.into_iter() {
                // Update the tx counts for this block
                for tx_hash in block.ethereum_block_information.transaction_hashes.iter() {
                    let Some((step_path, _)) = transaction_registration_information.get(tx_hash)
                    else {
                        continue;
                    };
                    *block.tx_counts.entry(step_path.clone()).or_default() += 1;
                }

                // Report information about the transactions within a block.
                for tx_hash in block.ethereum_block_information.transaction_hashes.iter() {
                    let Some((step_path, submission_time)) =
                        transaction_registration_information.get(tx_hash)
                    else {
                        continue;
                    };
                    let transaction_information = TransactionInformation {
                        transaction_hash: *tx_hash,
                        submission_timestamp: submission_time
                            .duration_since(UNIX_EPOCH)
                            .expect("Can't fail")
                            .as_secs() as _,
                        block_timestamp: block.ethereum_block_information.block_timestamp,
                        block_number: block.ethereum_block_information.block_number,
                    };
                    self.reporter
                        .report_step_transaction_information_event(
                            step_path.clone(),
                            transaction_information,
                        )
                        .expect("Can't fail")
                }

                // Report the block to the reporter.
                let _ = self.reporter.report_block_mined_event(block);
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
