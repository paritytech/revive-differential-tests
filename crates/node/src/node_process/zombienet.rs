use crate::internal_prelude::*;

#[derive(Debug)]
pub struct ZombienetProcess {
    network: Option<ZombienetNetwork<LocalFileSystem>>,
    // TODO(async): This is needed for now since we need to keep the runtime which zombienet was
    // spawned on alive until it's killed but we should remove this once we do the async rework.
    runtime: Option<Runtime>,
    url: String,
}

impl ZombienetProcess {
    pub fn new(network_configuration: NetworkConfig) -> anyhow::Result<Self> {
        let runtime =
            Runtime::new().context("Failed to create the tokio runtime that zombienet runs on")?;
        let network = runtime.block_on(async move {
            network_configuration
                .spawn_native()
                .await
                .context("Failed to spawn the zombienet network")
        })?;

        let url = network
            .parachains()
            .first()
            .context("No parachains in the zombienet config; can't get rpc URL")?
            .collators()
            .first()
            .context("No collators for the parachain; can't get the RPC url")?
            .ws_uri()
            .to_owned();

        Self {
            network: network.into(),
            runtime: runtime.into(),
            url,
        }
        .wait_for_first_3_blocks()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn wait_for_first_3_blocks(self) -> anyhow::Result<Self> {
        let task = async {
            let client = OnlineClient::<PolkadotConfig>::from_url(self.url.as_str())
                .await
                .context("Client creation failed")?;
            let mut block_subscription = client
                .blocks()
                .subscribe_finalized()
                .await
                .context("Block subscription failed")?;
            while let Some(block) = block_subscription.next().await {
                let block = block.context("Failed to get block")?;
                if block.number() > 3 {
                    return Ok(());
                }
            }
            bail!("Zombienet subscription closed before we observed the first block")
        };
        self.runtime
            .as_ref()
            .expect("qed; always called when runtime is available")
            .block_on(task)
            .context("Zombienet startup failed while watching for blocks")?;

        Ok(self)
    }
}

impl Drop for ZombienetProcess {
    fn drop(&mut self) {
        let (Some(network), Some(runtime)) = (self.network.take(), self.runtime.take()) else {
            return;
        };

        let join_handle = std::thread::spawn(move || {
            runtime.block_on(async move {
                if let Err(error) = network.destroy().await {
                    tracing::warn!("Failed to destroy zombienet network: {error:?}");
                }
            });
        });

        let _ = join_handle.join();
    }
}
