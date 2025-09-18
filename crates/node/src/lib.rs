//! This crate implements the testing nodes.

use alloy::genesis::Genesis;
use revive_dt_node_interaction::EthereumNode;

pub mod common;
pub mod constants;
pub mod geth;
pub mod substrate;

/// An abstract interface for testing nodes.
pub trait Node: EthereumNode {
    /// Spawns a node configured according to the genesis json.
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()>;

    /// Prune the node instance and related data.
    ///
    /// Blocking until it's completely stopped.
    fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Returns the node version.
    fn version(&self) -> anyhow::Result<String>;
}
