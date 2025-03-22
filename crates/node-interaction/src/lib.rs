//! This crate implements all node interactions.

use alloy::rpc::types::trace::geth::GethTrace;
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use revive_dt_node::Node;
use tokio_runtime::TO_TOKIO;

mod tokio_runtime;
pub mod trace;
pub mod transaction;

/// An interface for all node interactions.
pub trait NodeInteraction: Node {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction_request: TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        transaction_receipt: TransactionReceipt,
    ) -> anyhow::Result<GethTrace>;
}
