//! This crate implements all node interactions.

use alloy::primitives::Address;
use alloy::rpc::types::trace::geth::{DiffMode, GethTrace};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use tokio_runtime::TO_TOKIO;

pub mod nonce;
mod tokio_runtime;
pub mod trace;
pub mod transaction;

/// An interface for all interactions with Ethereum compatible nodes.
pub trait EthereumNode {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(&self, transaction: TransactionReceipt) -> anyhow::Result<GethTrace>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, transaction: TransactionReceipt) -> anyhow::Result<DiffMode>;

    /// Returns the next available nonce for the given [Address].
    fn fetch_add_nonce(&self, address: Address) -> anyhow::Result<u64>;
}
