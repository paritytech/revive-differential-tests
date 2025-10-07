//! This crate implements concurrent handling of testing node.

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context as _;
use revive_dt_config::*;
use crate::Platform;
use revive_dt_node_interaction::EthereumNode;

/// The node pool starts one or more [Node] which then can be accessed
/// in a round robbin fashion.
pub struct NodePool {
    next: AtomicUsize,
    nodes: Vec<Box<dyn EthereumNode + Send + Sync>>,
}

impl NodePool {
    /// Create a new Pool. This will start as many nodes as there are workers in `config`.
    pub async fn new(context: Context, platform: &dyn Platform) -> anyhow::Result<Self> {
        let concurrency_configuration = AsRef::<ConcurrencyConfiguration>::as_ref(&context);
        let nodes = concurrency_configuration.number_of_nodes;

        let mut handles = Vec::with_capacity(nodes);
        for _ in 0..nodes {
            let context = context.clone();
            handles.push(platform.new_node(context)?);
        }

        let mut nodes = Vec::with_capacity(nodes);
        for handle in handles {
            nodes.push(
                handle
                    .join()
                    .map_err(|error| anyhow::anyhow!("failed to spawn node: {:?}", error))
                    .context("Failed to join node spawn thread")?
                    .context("Node failed to spawn")?,
            );
        }

        let pre_transactions_tasks = nodes
            .iter_mut()
            .map(|node| node.pre_transactions())
            .collect::<Vec<_>>();
        futures::future::try_join_all(pre_transactions_tasks)
            .await
            .context("Failed to run the pre-transactions task")?;

        Ok(Self {
            nodes,
            next: Default::default(),
        })
    }

    /// Get a handle to the next node.
    pub fn round_robbin(&self) -> &dyn EthereumNode {
        let current = self.next.fetch_add(1, Ordering::SeqCst) % self.nodes.len();
        self.nodes.get(current).unwrap().as_ref()
    }
}
