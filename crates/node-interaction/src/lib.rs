//! This crate implements all node interactions.

use std::time::Duration;

use alloy::primitives::{Address, StorageKey, TxHash, U256};
use alloy::providers::ext::DebugApi;
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::trace::geth::{
    DiffMode, GethDebugTracingOptions, GethTrace, PreStateConfig, PreStateFrame,
};
use alloy::rpc::types::{EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest};
use anyhow::{Context as _, Result};

use futures::StreamExt;
use revive_common::EVMVersion;
use revive_dt_common::futures::{FrameworkFuture, FrameworkStream};

use revive_dt_common::subscriptions::{
    EthereumMinedBlockInformation, MinedBlockInformation, SubstrateMinedBlockInformation,
};
use subxt::{OnlineClient, SubstrateConfig};

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
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .send_transaction(transaction)
                .await
                .context("Failed to submit transaction")?
                .get_receipt()
                .await
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
        let provider = self.provider();
        let substrate_provider = self.substrate_provider();

        if let Some(substrate_provider) = substrate_provider {
            Box::pin(async move {
                let substrate_provider = substrate_provider.await?;
                let provider = provider.await.context("Failed to get the provider")?;
                let stream: FrameworkStream<MinedBlockInformation> = Box::pin(
                    substrate_provider
                        .blocks()
                        .subscribe_best()
                        .await
                        .context("Failed to create the block stream")?
                        .filter_map(move |block| {
                            let api = substrate_provider.clone();
                            let provider = provider.clone();

                            async move {
                                let substrate_block = block.ok()?;
                                let mut interval =
                                    tokio::time::interval(Duration::from_millis(250));
                                let revive_block = loop {
                                    interval.tick().await;
                                    let result = provider
                                        .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(
                                            substrate_block.number() as _,
                                        ))
                                        .await;
                                    if let Ok(Some(block)) = result {
                                        break block;
                                    }
                                };

                                let used = api
                                    .storage()
                                    .at(substrate_block.reference())
                                    .fetch_or_default(
                                        &revive_metadata::storage().system().block_weight(),
                                    )
                                    .await
                                    .expect("TODO: Remove");

                                let block_ref_time = (used.normal.ref_time as u128)
                                    + (used.operational.ref_time as u128)
                                    + (used.mandatory.ref_time as u128);
                                let block_proof_size = (used.normal.proof_size as u128)
                                    + (used.operational.proof_size as u128)
                                    + (used.mandatory.proof_size as u128);

                                let limits = api
                                    .constants()
                                    .at(&revive_metadata::constants().system().block_weights())
                                    .expect("TODO: Remove");

                                let max_ref_time = limits.max_block.ref_time;
                                let max_proof_size = limits.max_block.proof_size;

                                Some(MinedBlockInformation {
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
                                    substrate_block_information: Some(
                                        SubstrateMinedBlockInformation {
                                            ref_time: block_ref_time,
                                            max_ref_time,
                                            proof_size: block_proof_size,
                                            max_proof_size,
                                        },
                                    ),
                                    tx_counts: Default::default(),
                                })
                            }
                        }),
                );
                Ok(stream)
            })
        } else {
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
                            })
                        }),
                );
                Ok(stream)
            })
        }
    }

    /// A function to run post spawning the nodes and before any transactions are run on the node.
    fn pre_transactions(&mut self) -> FrameworkFuture<Result<()>> {
        Box::pin(async move { Ok(()) })
    }

    /// The alloy driver connected to the node's Rpc.
    fn provider(&self) -> FrameworkFuture<Result<DynProvider>>;

    /// The substrate provider used by the node. None if it's not a substrate node.
    fn substrate_provider(
        &self,
    ) -> Option<FrameworkFuture<Result<OnlineClient<SubstrateConfig>>>> {
        None
    }
}
