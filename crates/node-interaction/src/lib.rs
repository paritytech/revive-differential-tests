//! This crate implements all node interactions.

pub mod prelude {
    pub use crate::NodeApi;
    pub use crate::revive_metadata;
}

use std::collections::HashMap;
use std::future::ready;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, StorageKey, TxHash, U256, keccak256};
use alloy::providers::ext::DebugApi;
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::trace::geth::{
    DiffMode, GethDebugTracingOptions, GethTrace, PreStateConfig, PreStateFrame,
};
use alloy::rpc::types::{EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest};
use anyhow::{Context as _, Result};

use futures::future::Either;
use futures::{Stream, StreamExt, stream};
use pallet_revive_eth_rpc::ReceiptExtractor;
use revive_common::EVMVersion;
use revive_dt_common::futures::{FrameworkFuture, FrameworkStream, retry_with_exponential_backoff};

use revive_dt_common::subscriptions::{
    EthereumMinedBlockInformation, MinedBlockInformation, SubstrateMinedBlockInformation,
};
use subxt::utils::H256;
use subxt::{OnlineClient, PolkadotConfig};
use tokio::sync::RwLock;
use tokio::task::AbortHandle;
use tokio::time::interval;
use tracing::{debug, error, warn};

#[subxt::subxt(runtime_metadata_path = "../../assets/revive_metadata.scale")]
pub mod revive_metadata {}

/// An interface for all interactions with Ethereum compatible nodes.
#[allow(clippy::type_complexity)]
pub trait NodeApi {
    fn id(&self) -> usize;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> &str;

    fn submit_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<TxHash>> {
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .send_transaction(transaction)
                .await
                .context("Failed to submit transaction")
                .map(|pending_transaction| *pending_transaction.tx_hash())
        })
    }

    fn get_receipt(&self, tx_hash: TxHash) -> FrameworkFuture<Result<TransactionReceipt>> {
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .get_transaction_receipt(tx_hash)
                .await
                .context("Failed to get the transaction receipt")?
                .context("Failed to get the transaction receipt")
        })
    }

    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<TransactionReceipt>> {
        let submission_future = self.submit_transaction(transaction);
        let provider = self.provider();
        Box::pin(async move {
            let tx_hash = submission_future.await?;
            let provider = provider.await.context("Failed to get the provider")?;
            provider
                .get_transaction_receipt(tx_hash)
                .await
                .context("Failed to get the transaction receipt")?
                .context("Failed to get the transaction receipt")
        })
    }

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> FrameworkFuture<Result<GethTrace>> {
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .debug_trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to get the transaction trace")
        })
    }

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, tx_hash: TxHash) -> FrameworkFuture<Result<DiffMode>> {
        let provider = self.provider();
        Box::pin(async move {
            let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
                diff_mode: Some(true),
                disable_code: None,
                disable_storage: None,
            });
            match provider
                .await
                .context("Failed to get the provider")?
                .debug_trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to trace transaction for prestate diff")?
                .try_into_pre_state_frame()
                .context("Failed to convert trace into pre-state frame")?
            {
                PreStateFrame::Diff(diff) => Ok(diff),
                _ => anyhow::bail!("expected a diff mode trace"),
            }
        })
    }

    /// Returns the balance of the provided [`Address`] back.
    fn balance_of(&self, address: Address) -> FrameworkFuture<Result<U256>> {
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .get_balance(address)
                .await
                .context("Failed to get account balance")
        })
    }

    /// Returns the latest storage proof of the provided [`Address`]
    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> FrameworkFuture<Result<EIP1186AccountProofResponse>> {
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .get_proof(address, keys)
                .latest()
                .await
                .context("Failed to get state proof")
        })
    }

    /// Returns the EVM version of the node.
    fn evm_version(&self) -> EVMVersion;

    /// Returns a stream of the blocks that were mined by the node.
    fn subscribe_to_full_blocks_information(
        &self,
    ) -> FrameworkFuture<Result<FrameworkStream<MinedBlockInformation>>> {
        let provider = self.subscriptions_provider();
        let substrate_provider = self.substrate_provider();

        let future = if let Some(substrate_provider) = substrate_provider {
            Either::Left(subscribe_to_full_blocks_information_substrate(
                provider,
                substrate_provider,
            ))
        } else {
            Either::Right(subscribe_to_full_blocks_information_ethereum(provider))
        };
        Box::pin(future) as _
    }

    /// Subscribes to transaction hashes included in the blocks.
    ///
    /// The default implementation of this function uses a different strategy between the Ethereum
    /// nodes and the Substrate based nodes.
    ///
    /// For Ethereum node, we subscribe to blocks and then flatten their included transaction hashes
    /// into the stream and return them.
    ///
    /// For Substrate based nodes we subscribe to the best blocks, filter for the [`EthTransact`]
    /// extrinsics included in there, compute the transaction hashes from them, and then we return
    /// these hashes in the stream.
    ///
    /// The key thing in this implementation is that it makes some assumptions about how the down
    /// stream clients will use it. It assumes that the clients do not rely on finalized blocks to
    /// determine transaction inclusion and are fine with transactions merely being included in best
    /// blocks.
    fn subscribe_to_transaction_inclusions(
        &self,
    ) -> FrameworkFuture<Result<FrameworkStream<TxHash>>> {
        let provider = self.subscriptions_provider();
        let substrate_provider = self.substrate_provider();

        let future = if let Some(substrate_provider) = substrate_provider {
            Either::Left(subscribe_to_transaction_inclusions_substrate(
                substrate_provider,
            ))
        } else {
            Either::Right(subscribe_to_transaction_inclusions_ethereum(provider))
        };
        Box::pin(future) as _
    }

    /// Subscribes to the transaction receipts of each finalized block.
    ///
    /// The default implementation uses a different strategy between Ethereum and Substrate nodes.
    ///
    /// For Ethereum nodes, we subscribe to the finalized blocks stream and fetch all receipts
    /// for each block using [`eth_getBlockReceipts`], then flatten them into the output stream.
    ///
    /// For Substrate based nodes, we subscribe to the finalized blocks stream, extract the
    /// transaction hashes from each block, and then fetch individual receipts for each
    /// transaction via [`eth_getTransactionReceipt`] through the eth-rpc proxy. The receipts
    /// are collected per-block and then flattened into the output stream.
    ///
    /// In both cases, blocks whose receipts fail to be retrieved are silently skipped.
    fn subscribe_to_transaction_receipts(
        &self,
    ) -> FrameworkFuture<Result<FrameworkStream<TransactionReceipt>>> {
        let provider = self.subscriptions_provider();
        let substrate_provider = self.substrate_provider();

        let future = if let Some(substrate_provider) = substrate_provider {
            Either::Left(subscribe_to_transaction_receipts_substrate(
                substrate_provider,
            ))
        } else {
            Either::Right(subscribe_to_transaction_receipts_ethereum(provider))
        };
        Box::pin(future) as _
    }

    /// A function to run post spawning the nodes and before any transactions are run on the node.
    fn pre_transactions(&mut self) -> FrameworkFuture<Result<()>> {
        Box::pin(async move { Ok(()) })
    }

    /// The alloy provider connected to the node's Rpc.
    fn provider(&self) -> FrameworkFuture<Result<DynProvider>>;

    /// The alloy provider we use for all of the subscriptions through this node.
    fn subscriptions_provider(&self) -> FrameworkFuture<Result<DynProvider>> {
        self.provider()
    }

    /// The substrate provider used by the node. None if it's not a substrate node.
    fn substrate_provider(&self) -> Option<FrameworkFuture<Result<OnlineClient<PolkadotConfig>>>> {
        None
    }
}

fn subscribe_to_full_blocks_information_ethereum(
    provider: FrameworkFuture<Result<DynProvider>>,
) -> FrameworkFuture<Result<FrameworkStream<MinedBlockInformation>>> {
    Box::pin(async move {
        let stream: FrameworkStream<MinedBlockInformation> = Box::pin(
            provider
                .await
                .context("Failed to get the provider")?
                .subscribe_full_blocks()
                .into_stream()
                .await
                .context("Failed to create the block stream")?
                .filter_map(|block| async {
                    let block = block.ok()?;
                    Some(MinedBlockInformation {
                        ethereum_block_information: EthereumMinedBlockInformation {
                            block_number: block.number(),
                            block_timestamp: block.header.timestamp,
                            mined_gas: block.header.gas_used as _,
                            block_gas_limit: block.header.gas_limit as _,
                            transaction_hashes: block
                                .transactions
                                .into_hashes()
                                .as_hashes()
                                .expect("Must be hashes")
                                .to_vec(),
                        },
                        substrate_block_information: None,
                        tx_counts: Default::default(),
                        observation_time: SystemTime::now(),
                    })
                }),
        );
        Ok(stream)
    })
}

#[allow(clippy::type_complexity)]
fn subscribe_to_full_blocks_information_substrate(
    provider: FrameworkFuture<Result<DynProvider>>,
    substrate_provider: FrameworkFuture<Result<OnlineClient<PolkadotConfig>>>,
) -> FrameworkFuture<Result<FrameworkStream<MinedBlockInformation>>> {
    Box::pin(async move {
        let provider = provider.await.context("Failed to get provider")?;
        let substrate_provider = substrate_provider.await.context("Failed to get provider")?;

        // This is a map of the hash of the any substrate blocks to the time at which they were
        // observed. We use this as the canonical way of getting the "observation time" of blocks
        // on the chain. If we encounter a finalized block which doesn't have an associated time of
        // observation then we take the current time as the observation time.
        let observed_any_blocks = Arc::new(RwLock::new(HashMap::<H256, SystemTime>::new()));

        // This task's main objective is to subscribe to the any blocks and to process them as fast
        // as it can adding them to the observed any blocks map.
        let any_block_processing_task = tokio::spawn({
            let observed_any_blocks = observed_any_blocks.clone();
            let substrate_provider = substrate_provider.clone();
            async move {
                let stream = match substrate_provider.blocks().subscribe_all().await {
                    Ok(stream) => stream,
                    Err(err) => {
                        error!(?err, "Failed to subscribe to the any blocks, stopped");
                        return;
                    }
                };
                stream
                    .filter_map(|block| {
                        futures::future::ready(match block {
                            Ok(block) => Some(block),
                            Err(err) => {
                                error!(
                                    ?err,
                                    "Failed to get one of the blocks from the subscription"
                                );
                                None
                            }
                        })
                    })
                    .for_each(|block| {
                        let observed_any_blocks = observed_any_blocks.clone();
                        async move {
                            let observation_time = SystemTime::now();
                            debug!(
                                block.number = block.number(),
                                block.hash = ?block.hash(),
                                block.observation_time = observation_time.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                                "Observed a new any block"
                            );
                            let mut write_guard = observed_any_blocks
                                .write()
                                .await;
                            let entry = write_guard.entry(block.hash()).or_insert(observation_time);
                            *entry = (*entry).min(observation_time)
                        }
                    })
                    .await;
            }
        });

        // This is the main stream we return to the users. It's a stream over the finalized blocks
        // we observe from the subscription to the substrate rpc.
        let limits = substrate_provider
            .constants()
            .at(&revive_metadata::constants().system().block_weights())
            .expect("TODO: Remove")
            .per_class
            .normal
            .max_extrinsic
            .expect("TODO: Remove");

        let max_ref_time = limits.ref_time;
        let max_proof_size = limits.proof_size;
        let stream = substrate_provider
            .blocks()
            .subscribe_finalized()
            .await
            .context("Failed to subscribe to the finalized blocks")?
            .filter_map(|block| futures::future::ready(block.ok()))
            .map(move |substrate_block| {
                let provider = provider.clone();
                let substrate_provider = substrate_provider.clone();
                let observed_best_blocks = observed_any_blocks.clone();

                async move {
                    // Getting the observation time for this block from the map ob the observed best
                    // blocks, we will add this to the mined block information later on.
                    let observation_time = observed_best_blocks
                        .read()
                        .await
                        .get(&substrate_block.hash())
                        .copied()
                        .unwrap_or_else(SystemTime::now);

                    debug!(
                        block.number = substrate_block.number(),
                        block.hash = ?substrate_block.hash(),
                        block.observation_time = observation_time.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                        "Observed a new finalized block"
                    );

                    // Obtain the equivalent block to this block from the revive-eth-rpc. We do some
                    // polling logic here in order to ensure that if the rpc is yet to catch up we
                    // can still service the request.
                    let revive_block = {
                        let mut interval = tokio::time::interval(Duration::from_millis(250));
                        loop {
                            interval.tick().await;
                            let result = provider
                                .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(
                                    substrate_block.number() as _,
                                ))
                                .await;
                            if let Ok(Some(block)) = result {
                                break block;
                            }
                        }
                    };

                    // Constructing the block information.
                    let used = {
                        let mut interval = interval(Duration::from_secs(1));
                        loop {
                            interval.tick().await;

                            let result = substrate_provider
                            .storage()
                            .at(substrate_block.reference())
                            .fetch_or_default(&revive_metadata::storage().system().block_weight())
                            .await
                            .inspect_err(|err| warn!(?err, "Failed to get the substrate block weights"));

                            if let Ok(result) = result {
                                break result;
                            }
                        }
                    };

                    let block_ref_time = (used.normal.ref_time as u128)
                        + (used.operational.ref_time as u128)
                        + (used.mandatory.ref_time as u128);
                    let block_proof_size = (used.normal.proof_size as u128)
                        + (used.operational.proof_size as u128)
                        + (used.mandatory.proof_size as u128);

                    MinedBlockInformation {
                        ethereum_block_information: EthereumMinedBlockInformation {
                            block_number: revive_block.number(),
                            block_timestamp: revive_block.header.timestamp,
                            mined_gas: revive_block.header.gas_used as _,
                            block_gas_limit: revive_block.header.gas_limit as _,
                            transaction_hashes: revive_block
                                .transactions
                                .into_hashes()
                                .as_hashes()
                                .expect("Must be hashes")
                                .to_vec(),
                        },
                        substrate_block_information: Some(SubstrateMinedBlockInformation {
                            ref_time: block_ref_time,
                            max_ref_time,
                            proof_size: block_proof_size,
                            max_proof_size,
                        }),
                        tx_counts: Default::default(),
                        observation_time,
                    }
                }
            })
            .buffered(10);

        Ok(Box::pin(SubstrateSubscriptionStream {
            stream: Box::pin(stream),
            task_handle: any_block_processing_task.abort_handle(),
        }) as _)
    })
}

struct SubstrateSubscriptionStream<T> {
    stream: T,
    task_handle: AbortHandle,
}

impl<T> Drop for SubstrateSubscriptionStream<T> {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

impl<T> Stream for SubstrateSubscriptionStream<T>
where
    T: Stream + Unpin,
{
    type Item = T::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.stream).poll_next(cx)
    }
}

fn subscribe_to_transaction_inclusions_ethereum(
    provider: FrameworkFuture<Result<DynProvider>>,
) -> FrameworkFuture<Result<FrameworkStream<TxHash>>> {
    Box::pin(async move {
        let stream = provider
            .await
            .context("Failed to get the provider")?
            .subscribe_full_blocks()
            .into_stream()
            .await
            .context("Failed to subscribe to blocks")?
            .filter_map(|block| ready(block.ok()))
            .flat_map(|block| stream::iter(block.into_hashes_vec()));
        Ok(Box::pin(stream) as _)
    })
}

fn subscribe_to_transaction_inclusions_substrate(
    provider: FrameworkFuture<Result<OnlineClient<PolkadotConfig>>>,
) -> FrameworkFuture<Result<FrameworkStream<TxHash>>> {
    Box::pin(async move {
        let stream = provider
            .await
            .context("Failed to get the provider")?
            .blocks()
            .subscribe_all()
            .await
            .context("Failed to subscribe to blocks")?
            .filter_map(|block| ready(block.ok()))
            .then(|block| async move { block.extrinsics().await })
            .filter_map(|extrinsics| ready(extrinsics.ok()))
            .flat_map(move |extrinsics| {
                stream::iter(
                    extrinsics
                        .find::<revive_metadata::revive::calls::types::EthTransact>()
                        .filter_map(|ext| ext.ok())
                        .map(|ext| keccak256(&ext.value.payload))
                        .collect::<Vec<_>>(),
                )
            });

        Ok(Box::pin(stream) as _)
    })
}

fn subscribe_to_transaction_receipts_ethereum(
    provider: FrameworkFuture<Result<DynProvider>>,
) -> FrameworkFuture<Result<FrameworkStream<TransactionReceipt>>> {
    Box::pin(async move {
        let provider = provider.await.context("Failed to get the provider")?;

        let finalized_blocks_stream =
            subscribe_to_full_blocks_information_ethereum(Box::pin(ready(Ok(provider.clone()))))
                .await
                .context("Failed to get the finalized blocks stream")?;

        let receipts_stream = finalized_blocks_stream
            .map(|block| block.ethereum_block_information.block_number)
            .then(move |block_number| {
                let provider = provider.clone();
                async move {
                    let block_receipts = provider
                        .get_block_receipts(alloy::eips::BlockId::Number(
                            alloy::eips::BlockNumberOrTag::Number(block_number),
                        ))
                        .await;
                    let Ok(Some(receipts)) = block_receipts else {
                        error!(
                            block_number,
                            result = ?block_receipts,
                            "Failed to get the block receipts"
                        );
                        return None;
                    };
                    Some(receipts)
                }
            })
            .filter_map(ready)
            .flat_map(stream::iter);

        Ok(Box::pin(receipts_stream) as _)
    })
}

fn subscribe_to_transaction_receipts_substrate(
    substrate_provider: FrameworkFuture<Result<OnlineClient<PolkadotConfig>>>,
) -> FrameworkFuture<Result<FrameworkStream<TransactionReceipt>>> {
    Box::pin(async move {
        let substrate_provider = substrate_provider
            .await
            .context("Failed to get the substrate provider")?;

        let receipt_extractor = ReceiptExtractor::new(substrate_provider.clone(), None)
            .await
            .context("Failed to create the receipt extractor")
            .map(Arc::new)?;

        let receipts_stream = substrate_provider
            .blocks()
            .subscribe_finalized()
            .await
            .context("Failed to subscribe to finalized blocks")?
            .filter_map(|block| ready(block.ok()))
            .map(move |block| {
                let receipt_extractor = receipt_extractor.clone();
                async move {
                    let block_number = block.number();
                    let receipts =
                        retry_with_exponential_backoff(10, Duration::from_millis(500), || async {
                            match receipt_extractor.extract_from_block(&block).await {
                                Ok(receipts) => ControlFlow::Break(receipts),
                                Err(error) => ControlFlow::Continue(error),
                            }
                        })
                        .await
                        .with_context(|| {
                            format!("Failed to get the receipts for block: {block_number}")
                        })
                        .expect("Failed to get the receipts for the block");
                    Some(receipts.into_iter().map(|(_, receipt)| {
                        let serialized = serde_json::to_vec(&receipt).expect("Can't fail");
                        serde_json::from_slice::<TransactionReceipt>(&serialized)
                            .expect("Can't fail")
                    }))
                }
            })
            .buffered(10)
            .filter_map(ready)
            .flat_map(stream::iter);

        Ok(Box::pin(receipts_stream) as _)
    })
}
