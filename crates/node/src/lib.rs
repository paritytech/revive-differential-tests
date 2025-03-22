//! This crate implements the testing nodes.

use alloy::{
    genesis::Genesis,
    rpc::types::{TransactionReceipt, trace::geth::DiffMode},
};
use revive_dt_node_interaction::EthereumNode;

pub mod geth;

/// An abstract interface for testing nodes.
pub trait Node: EthereumNode {
    /// Spawns a node configured according to the [Genesis].
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self>;

    /// Prune the node instance and related data.
    ///
    /// Blocking until it's completely stopped.
    fn shutdown(self) -> anyhow::Result<()>;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> String;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, transaction: TransactionReceipt) -> anyhow::Result<DiffMode>;
}
