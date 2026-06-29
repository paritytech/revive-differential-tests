use crate::internal_prelude::*;

#[derive(Debug)]
pub struct ZombienetProcess {
    running: Option<RunningZombienet>,
    url: String,
}

impl ZombienetProcess {
    pub fn new(
        network_configuration: NetworkConfig,
        block_production_timeout: Duration,
        use_kubernetes: bool,
    ) -> Result<Self> {
        let runtime =
            Runtime::new().context("Failed to create the tokio runtime that zombienet runs on")?;
        let network = runtime.block_on(async move {
            if use_kubernetes {
                network_configuration
                    .spawn_k8s()
                    .await
                    .context("Failed to spawn the zombienet network")
            } else {
                network_configuration
                    .spawn_native()
                    .await
                    .context("Failed to spawn the zombienet network")
            }
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
            running: RunningZombienet { network, runtime }.into(),
            url,
        }
        .wait_for_finalized_blocks(block_production_timeout)
    }

    fn wait_for_finalized_blocks(self, block_production_timeout: Duration) -> Result<Self> {
        let connection_string = self.url.clone();
        let task = async move {
            let client = OnlineClient::<PolkadotConfig>::from_url(connection_string.as_str())
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
            bail!("Zombienet subscription closed before we observed finalized blocks")
        };
        self.running
            .as_ref()
            .context("Zombienet process is not running")?
            .runtime
            .block_on(async move { tokio::time::timeout(block_production_timeout, task).await })
            .context("Timed out while waiting for zombienet to produce its first block(s)")?
            .context("Zombienet startup failed while watching for blocks")?;

        Ok(self)
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Debug)]
struct RunningZombienet {
    network: ZombienetNetwork<LocalFileSystem>,
    runtime: Runtime,
}

impl Drop for ZombienetProcess {
    fn drop(&mut self) {
        let Some(RunningZombienet { network, runtime }) = self.running.take() else {
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
