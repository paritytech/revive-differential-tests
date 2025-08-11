//! This crate implements all node interactions.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::trace::geth::{DiffMode, GethDebugTracingOptions, GethTrace};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use anyhow::Result;

/// An interface for all interactions with Ethereum compatible nodes.
pub trait EthereumNode {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> impl Future<Output = Result<TransactionReceipt>>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        receipt: &TransactionReceipt,
        trace_options: GethDebugTracingOptions,
    ) -> impl Future<Output = Result<GethTrace>>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, receipt: &TransactionReceipt) -> impl Future<Output = Result<DiffMode>>;

    /// Returns the balance of the provided [`Address`] back.
    fn balance_of(&self, address: Address) -> impl Future<Output = Result<U256>>;
}
