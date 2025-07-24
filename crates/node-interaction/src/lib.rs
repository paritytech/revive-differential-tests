//! This crate implements all node interactions.

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, ChainId, U256};
use alloy::rpc::types::trace::geth::{DiffMode, GethDebugTracingOptions, GethTrace};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use anyhow::Result;

/// An interface for all interactions with Ethereum compatible nodes.
pub trait EthereumNode {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(&self, transaction: TransactionRequest) -> Result<TransactionReceipt>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        receipt: &TransactionReceipt,
        trace_options: GethDebugTracingOptions,
    ) -> Result<GethTrace>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, receipt: &TransactionReceipt) -> Result<DiffMode>;

    /// Returns the ID of the chain that the node is on.
    fn chain_id(&self) -> Result<ChainId>;

    // TODO: This is currently a u128 due to Kitchensink needing more than 64 bits for its gas limit
    // when we implement the changes to the gas we need to adjust this to be a u64.
    /// Returns the gas limit of the specified block.
    fn block_gas_limit(&self, number: BlockNumberOrTag) -> Result<u128>;

    /// Returns the coinbase of the specified block.
    fn block_coinbase(&self, number: BlockNumberOrTag) -> Result<Address>;

    /// Returns the difficulty of the specified block.
    fn block_difficulty(&self, number: BlockNumberOrTag) -> Result<U256>;

    /// Returns the hash of the specified block.
    fn block_hash(&self, number: BlockNumberOrTag) -> Result<BlockHash>;

    /// Returns the timestamp of the specified block,
    fn block_timestamp(&self, number: BlockNumberOrTag) -> Result<BlockTimestamp>;

    /// Returns the number of the last block.
    fn last_block_number(&self) -> Result<BlockNumber>;
}
