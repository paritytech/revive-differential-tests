use crate::internal_prelude::*;

#[derive(Debug)]
pub struct ReviveDevNodeProcess {
    _process: NodeProcess,
    url: String,
}

impl ReviveDevNodeProcess {
    const READY_MARKER: &str = "Running JSON-RPC server";

    pub fn new(
        binary_path: impl AsRef<Path>,
        chainspec_path: impl AsRef<Path>,
        consensus: impl AsRef<OsStr>,
        base_directory: impl AsRef<Path>,
        logs_directory: impl AsRef<Path>,
        logging_level: impl AsRef<OsStr>,
    ) -> anyhow::Result<Self> {
        let node_port =
            AllocatedPort::allocate().context("Failed to allocate port for the revive-dev-node")?;
        let url = format!("ws://127.0.0.1:{node_port}");

        let builder = NodeProcess::builder(binary_path.as_ref())
            .with_common_substrate_node_args(
                chainspec_path,
                base_directory,
                node_port,
                logging_level,
            )
            .arg("--dev")
            .arg("--force-authoring")
            .arg("--pool-type")
            .arg("single-state")
            .arg("--consensus")
            .arg(consensus)
            .log("revive_dev_node", logs_directory)
            .wait_for_startup(WaitForStartupSentinel {
                // TODO(better-config): Turn this into an argument
                timeout: Duration::from_secs(90),
                successful_startup_if_encountered: Self::READY_MARKER.into(),
                failed_startup_if_encountered: None::<Cow<'static, _>>,
            });

        let process = builder
            .build()
            .context("Failed to start the revive-dev-node process")?;

        Ok(Self {
            _process: process,
            url,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
