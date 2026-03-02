use futures::future::join;

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

    /// The reporter used to send events to the report aggregator.
    reporter: ExecutionSpecificReporter,
}

impl Watcher {
    pub fn new(
        blocks_stream: FrameworkStream<MinedBlockInformation>,
        reporter: ExecutionSpecificReporter,
    ) -> (Self, UnboundedSender<WatcherEvent>) {
        let (tx, rx) = unbounded_channel::<WatcherEvent>();
        (
            Self {
                rx,
                blocks_stream,
                reporter,
            },
            tx,
        )
    }

    /// The main future which should be polled in order for the watcher to run.
    ///
    /// This task spawns two tasks which work together until a final outcome is reached.
    ///
    /// 1. The first task listens for [`WatcherEvent`]s. For each transaction that it encounters it
    ///    adds its hash to a `watching_transaction_hashes` [`HashSet`] which means that the task is
    ///    watching for this transaction hash and waiting for it to be mined into a block. This is
    ///    akin to a user sending a request saying "please watch for this transaction hash for me"
    ///    and this task is the handler of such a request. All that it does is that it stores the
    ///    hash of this transaction in a shared [`HashSet`] for the other task to handle later on.
    ///    Additionally, when this task receives a [`WatcherEvent::AllTransactionsSubmitted`] event
    ///    it mutates a shared boolean which informs of the later task of the fact that submission
    ///    is completed.
    /// 2. A second task whose main objective is to watch for the blocks getting mined and note down
    ///    all of the transactions that it has observed in these blocks. It does so by storing them
    ///    in a [`HashSet`] of transaction hashes. Eventually, when the shared boolean denoting the
    ///    completion of submission is set to [`true`], this task would then compare its set of
    ///    observed transactions to the set of transactions that we wanted to watch for. If the set
    ///    has no differences it means that we've successfully observed all of the transactions we
    ///    wanted to observe and this task completes.
    ///
    /// The two spawned tasks are also involved in some amount of reporting to the reporter at
    /// various different stages in their runtime and they also return various values as tasks.
    ///
    /// The first task guarantees that no more transaction hashes will be requested to be watched
    /// for by closing the channel completely and stopping its own execution which ensures that the
    /// sender's side gets an error if it attempts to request for another transaction to be watched.
    ///
    /// This future resolves once the two above described tasks resolve and complete their execution
    /// based on the above described logic.
    #[instrument(level = "info", skip_all)]
    pub async fn run(mut self) -> Result<()> {
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
        let registration_task = {
            let is_submission_completed = is_submission_completed.clone();
            let watch_requests = watch_requests.clone();
            async move {
                let mut transaction_information = HashMap::new();
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
        };

        // This is the block subscription task which is the main watcher task watching for all of
        // the observed transactions and keeping track of them. It's implemented as described in the
        // doc comment of the function.
        let block_subscription_task = {
            let is_submission_completed = is_submission_completed.clone();
            let watch_requests = watch_requests.clone();
            async move {
                let mut observed_transaction_hashes = HashSet::new();
                let mut observed_blocks = vec![];
                while let Some(MinedBlockInformation {
                    ethereum_block_information: eth_block,
                    substrate_block_information: substrate_block,
                    tx_counts,
                }) = self.blocks_stream.next().await
                {
                    // Keep skipping block as long as their block number is below the block number
                    // we were tasked to start at.
                    if eth_block.block_number <= ignore_blocks_before_block_number {
                        info!(
                            block_number = eth_block.block_number,
                            ignore_blocks_before_block_number,
                            "Observed a block, but it's being ignored"
                        );
                        continue;
                    }

                    // Add the transaction hashes from this block to the set of transaction hashes
                    // we've observed and also add it's information to the map we store where we
                    // keep track of the transaction and which block contained it.
                    observed_transaction_hashes.extend(eth_block.transaction_hashes.clone());

                    // Logging information about this newly observed block and adding it to the set
                    // of block we will return at the end.
                    let requested_transactions_len = watch_requests.read().await.len();
                    info!(
                        block.number = eth_block.block_number,
                        block.timestamp = eth_block.block_timestamp,
                        block.tx_hashes_len = eth_block.transaction_hashes.len(),
                        transactions.observed = observed_transaction_hashes.len(),
                        transactions.requested = requested_transactions_len,
                        transactions.remaining = requested_transactions_len
                            .saturating_sub(observed_transaction_hashes.len()),
                        "Observed a new block"
                    );
                    observed_blocks.push(MinedBlockInformation {
                        ethereum_block_information: eth_block,
                        substrate_block_information: substrate_block,
                        tx_counts,
                    });

                    // This is the primary condition which determines if we should break out of the
                    // loop or not. If all of the transactions have been submitted and there's no
                    // difference between the transactions the user requested us to watch and the
                    // ones we've seen then we break out of the loop and stop.
                    if *is_submission_completed.read().await
                        && watch_requests
                            .read()
                            .await
                            .is_subset(&observed_transaction_hashes)
                    {
                        info!("All transactions observed");
                        break;
                    }
                }
                observed_blocks
            }
        };

        // Execute both of the tasks concurrently until they both complete.
        let (transaction_registration_information, observed_blocks) =
            join(registration_task, block_subscription_task).await;

        // Reporting all of the information to the reporter about all of the observed blocks, the tx
        // counts, the transaction information, and everything else.
        for mut block in observed_blocks.into_iter() {
            // Update the tx counts for this block
            for tx_hash in block.ethereum_block_information.transaction_hashes.iter() {
                let Some((step_path, _)) = transaction_registration_information.get(tx_hash) else {
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

        // // Iterate over all of the transactions in the registration information to submit additional
        // // data about them to the reporter. Note here that we're GUARANTEED to have the hash from
        // // the registration information in the other map since the futures have resolved.
        // for (transaction_hash, (step_path, submission_time)) in
        //     transaction_registration_information.into_iter()
        // {
        //     let Some((block_number, block_timestamp)) =
        //         transaction_observation_information.remove(&transaction_hash)
        //     else {
        //         unreachable!()
        //     };
        //     let transaction_information = TransactionInformation {
        //         transaction_hash,
        //         submission_timestamp: submission_time
        //             .duration_since(UNIX_EPOCH)
        //             .expect("Can't fail")
        //             .as_secs() as _,
        //         block_timestamp,
        //         block_number,
        //     };
        //     self.reporter
        //         .report_step_transaction_information_event(step_path, transaction_information)
        //         .expect("Can't fail")
        // }

        Ok(())
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
