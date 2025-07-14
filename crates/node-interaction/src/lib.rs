//! This crate implements all node interactions.

use alloy::primitives::{Address, ChainId};
use alloy::rpc::types::trace::geth::{DiffMode, GethTrace};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use anyhow::Result;

mod blocking_executor;
pub use blocking_executor::*;

/// An interface for all interactions with Ethereum compatible nodes.
pub trait EthereumNode {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(&self, transaction: TransactionRequest) -> Result<TransactionReceipt>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(&self, transaction: TransactionReceipt) -> Result<GethTrace>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, transaction: TransactionReceipt) -> Result<DiffMode>;

    /// Returns the next available nonce for the given [Address].
    fn fetch_add_nonce(&self, address: Address) -> Result<u64>;

    /// Returns the ID of the chain that the node is on.
    fn chain_id(&self) -> Result<ChainId>;

    // TODO: This is currently a u128 due to Kitchensink needing more than 64 bits for its gas limit
    // when we implement the changes to the gas we need to adjust this to be a u64.
    /// Returns the gas limit of the last block.
    fn gas_limit(&self) -> Result<u128>;

    /// Returns the coinbase of the last block.
    fn coinbase(&self) -> Result<Address>;
}
