use crate::internal_prelude::*;

#[derive(Debug)]
pub struct PolkadotOmniNodeProcess {
    _process: NodeProcess,
    url: String,
}

impl PolkadotOmniNodeProcess {
    const READY_MARKER: &str = "Running JSON-RPC server";

    pub fn new(
        binary_path: impl AsRef<Path>,
        block_time: Duration,
        chainspec_path: impl AsRef<Path>,
        base_directory: impl AsRef<Path>,
        logs_directory: impl AsRef<Path>,
        logging_level: impl AsRef<OsStr>,
    ) -> anyhow::Result<Self> {
        let node_port = PortAllocator::allocate_port()
            .context("Failed to allocate port for the polkadot-omni-node")?;

        let builder = NodeProcess::builder(binary_path.as_ref())
            .with_common_substrate_node_args(
                chainspec_path,
                base_directory,
                node_port,
                logging_level,
            )
            .arg("--dev-block-time")
            .arg(block_time.as_millis().to_string())
            .arg("--no-hardware-benchmarks")
            .arg("--authoring")
            .arg("slot-based")
            .log("polkadot_omni_node", logs_directory)
            .wait_for_startup(WaitForStartupSentinel {
                // TODO(better-config): Turn this into an argument
                timeout: Duration::from_secs(120),
                successful_startup_if_encountered: Self::READY_MARKER.into(),
                failed_startup_if_encountered: None::<Cow<'static, _>>,
            });

        let process = builder
            .build()
            .context("Failed to start the polkadot-omni-node process")?;

        Ok(Self {
            _process: process,
            url: format!("ws://127.0.0.1:{node_port}"),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
