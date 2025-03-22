//! An abstract interface for testing nodes.

use alloy::{
    genesis::Genesis,
    rpc::types::{TransactionReceipt, TransactionRequest, trace::parity::StateDiff},
};

pub trait Node {
    /// Configures the node with the given genesis configuration.
    fn with_genesis(&mut self, genesis: &Genesis) -> anyhow::Result<&mut Self>;

    /// Spawns the node, blocking until it's ready to accept transactions.
    fn spawn(&mut self) -> anyhow::Result<&mut Self>;

    /// Prune the node, blocking until it's completely stopped.
    fn shutdown(self) -> anyhow::Result<()>;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> String;

    /// Execute the [TransactionRequest], blocking until the transaction is mined.
    fn execute_transaction(
        &self,
        transaction: &TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, transaction: &TransactionReceipt) -> anyhow::Result<StateDiff>;
}
