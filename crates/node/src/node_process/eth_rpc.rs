use crate::internal_prelude::*;

#[derive(Debug)]
pub struct EthRpcProcess {
    _process: NodeProcess,
    url: String,
}

impl EthRpcProcess {
    const READY_MARKER: &str = "Running JSON-RPC server";

    pub fn new(
        binary_path: impl AsRef<Path>,
        logs_directory: impl AsRef<Path>,
        substrate_rpc_url: impl AsRef<OsStr>,
        logging_level: impl AsRef<OsStr>,
    ) -> anyhow::Result<Self> {
        let eth_rpc_port =
            PortAllocator::allocate_port().context("Failed to allocate port for the eth-rpc")?;

        let process = NodeProcess::builder(binary_path.as_ref())
            .arg("--dev")
            .arg("--node-rpc-url")
            .arg(substrate_rpc_url)
            .arg("--rpc-cors")
            .arg("all")
            .arg("--rpc-max-connections")
            .arg(u32::MAX.to_string())
            .arg("--rpc-port")
            .arg(eth_rpc_port.to_string())
            .arg("--rpc-max-batch-request-len")
            .arg(u32::MAX.to_string())
            .arg("--eth-pruning")
            .arg("archive")
            .log("eth_rpc", logs_directory)
            .env("RUST_LOG", logging_level)
            .wait_for_startup(WaitForStartupSentinel {
                // TODO(better-config): Turn this into an argument
                timeout: Duration::from_secs(30),
                successful_startup_if_encountered: Self::READY_MARKER.into(),
                failed_startup_if_encountered: None::<Cow<'static, str>>,
            })
            .build()
            .context("Failed to start the eth-rpc processes")?;

        Ok(Self {
            _process: process,
            url: format!("http://127.0.0.1:{eth_rpc_port}"),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
