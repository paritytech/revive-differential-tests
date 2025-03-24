//! This crate implements the testing nodes.

use alloy::rpc::types::{TransactionReceipt, trace::geth::DiffMode};
use revive_dt_config::Arguments;
use revive_dt_node_interaction::EthereumNode;

pub mod geth;

/// The default genesis configuration.
pub const GENESIS_JSON: &str = include_str!("../../../genesis.json");

/// An abstract interface for testing nodes.
pub trait Node: EthereumNode {
    /// Create a new uninitialized instance.
    fn new(config: &Arguments) -> Self;

    /// Spawns a node configured according to the genesis json.
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: String) -> anyhow::Result<()>;

    /// Prune the node instance and related data.
    ///
    /// Blocking until it's completely stopped.
    fn shutdown(self) -> anyhow::Result<()>;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> String;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, transaction: TransactionReceipt) -> anyhow::Result<DiffMode>;

    /// Returns the node version.
    fn version(&self) -> anyhow::Result<String>;
}
