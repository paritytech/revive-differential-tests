//! This crate implements concurrent handling of testing node.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use revive_dt_common::cached_fs::read_to_string;

use anyhow::Context;
use revive_dt_config::Arguments;

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
    pub fn new(config: &Arguments) -> anyhow::Result<Self> {
        let nodes = config.number_of_nodes;
        let genesis = read_to_string(&config.genesis_file).context(format!(
            "can not read genesis file: {}",
            config.genesis_file.display()
        ))?;

        let mut handles = Vec::with_capacity(nodes);
        for _ in 0..nodes {
            let config = config.clone();
            let genesis = genesis.clone();
            handles.push(thread::spawn(move || spawn_node::<T>(&config, genesis)));
        }

        let mut nodes = Vec::with_capacity(nodes);
        for handle in handles {
            nodes.push(
                handle
                    .join()
                    .map_err(|error| anyhow::anyhow!("failed to spawn node: {:?}", error))?
                    .map_err(|error| anyhow::anyhow!("node failed to spawn: {error}"))?,
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

fn spawn_node<T: Node + Send>(args: &Arguments, genesis: String) -> anyhow::Result<T> {
    let mut node = T::new(args);
    tracing::info!("starting node: {}", node.connection_string());
    node.spawn(genesis)?;
    Ok(node)
}
