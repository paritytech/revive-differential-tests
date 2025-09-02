//! This crate implements concurrent handling of testing node.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use alloy::genesis::Genesis;
use anyhow::Context as _;
use revive_dt_config::{
    ConcurrencyConfiguration, EthRpcConfiguration, GenesisConfiguration, GethConfiguration,
    KitchensinkConfiguration, ReviveDevNodeConfiguration, WalletConfiguration,
    WorkingDirectoryConfiguration,
};
use tracing::info;

use crate::Node;

/// The node pool starts one or more [Node] which then can be accessed
/// in a round robbin fasion.
pub struct NodePool<T> {
    next: AtomicUsize,
    nodes: Vec<T>,
}

impl<T> NodePool<T>
where
    T: Node + Send + 'static,
{
    /// Create a new Pool. This will start as many nodes as there are workers in `config`.
    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<ConcurrencyConfiguration>
        + AsRef<GenesisConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<GethConfiguration>
        + AsRef<KitchensinkConfiguration>
        + AsRef<ReviveDevNodeConfiguration>
        + AsRef<EthRpcConfiguration>
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> anyhow::Result<Self> {
        let concurrency_configuration = AsRef::<ConcurrencyConfiguration>::as_ref(&context);
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);

        let nodes = concurrency_configuration.number_of_nodes;
        let genesis = genesis_configuration.genesis()?;

        let mut handles = Vec::with_capacity(nodes);
        for _ in 0..nodes {
            let context = context.clone();
            let genesis = genesis.clone();
            handles.push(thread::spawn(move || spawn_node::<T>(context, genesis)));
        }

        let mut nodes = Vec::with_capacity(nodes);
        for handle in handles {
            nodes.push(
                handle
                    .join()
                    .map_err(|error| anyhow::anyhow!("failed to spawn node: {:?}", error))
                    .context("Failed to join node spawn thread")?
                    .map_err(|error| anyhow::anyhow!("node failed to spawn: {error}"))
                    .context("Node failed to spawn")?,
            );
        }

        Ok(Self {
            nodes,
            next: Default::default(),
        })
    }

    /// Get a handle to the next node.
    pub fn round_robbin(&self) -> &T {
        let current = self.next.fetch_add(1, Ordering::SeqCst) % self.nodes.len();
        self.nodes.get(current).unwrap()
    }
}

fn spawn_node<T: Node + Send>(
    context: impl AsRef<WorkingDirectoryConfiguration>
    + AsRef<ConcurrencyConfiguration>
    + AsRef<GenesisConfiguration>
    + AsRef<WalletConfiguration>
    + AsRef<GethConfiguration>
    + AsRef<KitchensinkConfiguration>
    + AsRef<ReviveDevNodeConfiguration>
    + AsRef<EthRpcConfiguration>
    + Clone
    + 'static,
    genesis: Genesis,
) -> anyhow::Result<T> {
    let mut node = T::new(context);
    info!(
        id = node.id(),
        connection_string = node.connection_string(),
        "Spawning node"
    );
    node.spawn(genesis)
        .context("Failed to spawn node process")?;
    info!(
        id = node.id(),
        connection_string = node.connection_string(),
        "Spawned node"
    );
    Ok(node)
}
