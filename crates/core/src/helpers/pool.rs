//! This crate implements concurrent handling of testing node.

use crate::internal_prelude::*;

/// The node pool starts one or more [Node] which then can be accessed
/// in a round robbin fashion.
pub struct NodePool {
    next: AtomicUsize,
    node_connectors: Vec<Arc<NodeConnector>>,
}

impl NodePool {
    /// Create a new Pool. This will start as many nodes as there are workers in `config`.
    pub async fn new(context: Context, platform: &dyn Platform) -> anyhow::Result<Self> {
        let concurrency_configuration = context.as_concurrency_configuration();
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

        let node_connectors = try_join_all(nodes)
            .await
            .context("Failed to start all of the node connectors")?
            .into_iter()
            .map(Arc::new)
            .collect();

        Ok(Self {
            node_connectors,
            next: Default::default(),
        })
    }

    /// Get a handle to the next node.
    pub fn round_robbin(&self) -> Arc<NodeConnector> {
        let current = self.next.fetch_add(1, Ordering::SeqCst) % self.node_connectors.len();
        self.node_connectors[current].clone()
    }
}
