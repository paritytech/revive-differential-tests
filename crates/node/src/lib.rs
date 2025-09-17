//! This crate implements the testing nodes.

use alloy::genesis::Genesis;
use revive_common::EVMVersion;
use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;

pub mod common;
pub mod constants;
pub mod geth;
pub mod kitchensink;
pub mod pool;

/// An abstract interface for testing nodes.
pub trait Node: EthereumNode {
    /// Returns the identifier of the node.
    fn id(&self) -> usize;

    /// Spawns a node configured according to the genesis json.
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()>;

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
    fn matches_target(targets: Option<&[String]>) -> bool;

    /// Returns the EVM version of the node.
    fn evm_version() -> EVMVersion;
}
