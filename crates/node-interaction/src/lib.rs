//! This crate implements all node interactions.

use std::pin::Pin;
use std::sync::Arc;

use alloy::primitives::{Address, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256};
use alloy::rpc::types::trace::geth::{DiffMode, GethDebugTracingOptions, GethTrace};
use alloy::rpc::types::{EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest};
use anyhow::Result;

use futures::Stream;
use revive_common::EVMVersion;
use revive_dt_format::traits::ResolverApi;

/// An interface for all interactions with Ethereum compatible nodes.
#[allow(clippy::type_complexity)]
pub trait EthereumNode {
    /// A function to run post spawning the nodes and before any transactions are run on the node.
    fn pre_transactions(&mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + '_>>;

    fn id(&self) -> usize;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> &str;

    fn submit_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TxHash>> + '_>>;

    fn get_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Pin<Box<dyn Future<Output = Result<TransactionReceipt>> + '_>>;

    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TransactionReceipt>> + '_>>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = Result<GethTrace>> + '_>>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, tx_hash: TxHash) -> Pin<Box<dyn Future<Output = Result<DiffMode>> + '_>>;

    /// Returns the balance of the provided [`Address`] back.
    fn balance_of(&self, address: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + '_>>;

    /// Returns the latest storage proof of the provided [`Address`]
    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> Pin<Box<dyn Future<Output = Result<EIP1186AccountProofResponse>> + '_>>;

    /// Returns the resolver that is to use with this ethereum node.
    fn resolver(&self) -> Pin<Box<dyn Future<Output = Result<Arc<dyn ResolverApi + '_>>> + '_>>;

    /// Returns the EVM version of the node.
    fn evm_version(&self) -> EVMVersion;

    /// Returns a stream of the blocks that were mined by the node.
    fn subscribe_to_full_blocks_information(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = anyhow::Result<Pin<Box<dyn Stream<Item = MinedBlockInformation>>>>>
                + '_,
        >,
    >;

    /// Creates a node instance from an existing running node.
    fn new_existing() -> Self
    where
        Self: Sized,
    {
        panic!("new_existing is not implemented for this node type")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinedBlockInformation {
    /// The block number.
    pub block_number: BlockNumber,

    /// The block timestamp.
    pub block_timestamp: BlockTimestamp,

    /// The amount of gas mined in the block.
    pub mined_gas: u128,

    /// The gas limit of the block.
    pub block_gas_limit: u128,

    /// The hashes of the transactions that were mined as part of the block.
    pub transaction_hashes: Vec<TxHash>,
}
