//! This crate implements concurrent handling of testing node.

use crate::internal_prelude::*;

/// The node pool starts one or more [Node] which then can be accessed
/// in a round robbin fashion.
pub struct NodePool {
    next: AtomicUsize,
    node_connectors: Vec<Arc<NodeConnector>>,
}

impl NodePool {
    /// Creates a pool containing the requested number of node connectors.
    pub async fn new(
        number_of_node_connectors: usize,
        new_node_connector: impl AsyncFn() -> anyhow::Result<NodeConnector>,
    ) -> anyhow::Result<Self> {
        info!("Awaiting node connectors to start");
        let node_connectors =
            try_join_all((0..number_of_node_connectors).map(|_| new_node_connector()))
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
