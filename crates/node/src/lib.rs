//! This crate implements the testing nodes.

use revive_dt_config::Arguments;
use revive_dt_node_interaction::EthereumNode;

pub mod common;
pub mod geth;
pub mod kitchensink;
pub mod pool;

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
    fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Returns the nodes connection string.
    fn connection_string(&self) -> String;

    /// Returns the node version.
    fn version(&self) -> anyhow::Result<String>;

    /// Given a list of targets from the metadata file, this function determines if the metadata
    /// file can be ran on this node or not.
    fn matches_target(&self, targets: Option<&[String]>) -> bool;
}
