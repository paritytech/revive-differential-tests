use std::{
    io::BufRead,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder, ext::DebugApi},
    rpc::types::{
        TransactionReceipt,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};

use crate::Node;
use revive_dt_config::Arguments;
use revive_dt_node_interaction::{
    EthereumNode, trace::trace_transaction, transaction::execute_transaction,
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct KitchensinkNode {
    id: u32,
    substrate_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    rpc_url: String,
    wallet: EthereumWallet,
    process_substrate: Option<Child>,
    process_proxy: Option<Child>,
}

impl KitchensinkNode {
    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    fn spawn_process(&mut self, _genesis: String) -> anyhow::Result<()> {
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        // Start Substrate node
        let mut substrate_process = Command::new(&self.substrate_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(substrate_rpc_port.to_string())
            .arg("--name")
            .arg(format!("revive-kitchensink-{}", self.id))
            .arg("--rpc-methods")
            .arg("Unsafe")
            .env(
                "RUST_LOG",
                "error,evm=debug,sc_rpc_server=info,runtime::revive=debug",
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Give the node a moment to boot
        Self::wait_ready(
            &mut substrate_process,
            Self::SUBSTRATE_READY_MARKER,
            Duration::from_secs(10),
        )?;

        let mut proxy_process = Command::new(&self.eth_proxy_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(proxy_rpc_port.to_string())
            .arg("--node-rpc-url")
            .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
            .env("RUST_LOG", "info,eth-rpc=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        Self::wait_ready(
            &mut proxy_process,
            Self::ETH_PROXY_READY_MARKER,
            Duration::from_secs(30),
        )?;

        self.process_substrate = Some(substrate_process);
        self.process_proxy = Some(proxy_process);

        Ok(())
    }

    fn wait_ready(child: &mut Child, marker: &str, timeout: Duration) -> anyhow::Result<()> {
        let start_time = std::time::Instant::now();
        let stderr = child.stderr.take().expect("stderr must be piped");

        let mut lines = std::io::BufReader::new(stderr).lines();
        loop {
            if let Some(Ok(line)) = lines.next() {
                if line.contains(marker) {
                    std::thread::spawn(move || for _ in lines.by_ref() {});
                    return Ok(());
                }
            }

            if start_time.elapsed() > timeout {
                let _ = child.kill();
                anyhow::bail!("Timeout waiting for process readiness: {marker}");
            }
        }
    }

    pub fn eth_rpc_version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.eth_proxy_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }
}

impl EthereumNode for KitchensinkNode {
    fn execute_transaction(
        &self,
        transaction: alloy::rpc::types::TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt> {
        let url = self.rpc_url.clone();
        let wallet = self.wallet.clone();

        execute_transaction(Box::pin(async move {
            Ok(ProviderBuilder::new()
                .wallet(wallet)
                .connect(&url)
                .await?
                .send_transaction(transaction)
                .await?
                .get_receipt()
                .await?)
        }))
    }

    fn trace_transaction(
        &self,
        transaction: TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let url = self.rpc_url.clone();
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });

        let wallet = self.wallet.clone();

        trace_transaction(Box::pin(async move {
            Ok(ProviderBuilder::new()
                .wallet(wallet)
                .connect(&url)
                .await?
                .debug_trace_transaction(transaction.transaction_hash, trace_options)
                .await?)
        }))
    }
}

impl Node for KitchensinkNode {
    fn new(config: &Arguments) -> Self {
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);

        Self {
            id,
            substrate_binary: config.kitchensink.clone(),
            eth_proxy_binary: config.eth_proxy.clone(),
            rpc_url: String::new(),
            wallet: config.wallet(),
            process_substrate: None,
            process_proxy: None,
        }
    }

    fn connection_string(&self) -> String {
        self.rpc_url.clone()
    }

    fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.process_proxy.take() {
            let _ = child.kill();
        }
        if let Some(mut child) = self.process_substrate.take() {
            let _ = child.kill();
        }
        Ok(())
    }

    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.spawn_process(genesis)
    }

    fn state_diff(&self, transaction: TransactionReceipt) -> anyhow::Result<DiffMode> {
        match self
            .trace_transaction(transaction)?
            .try_into_pre_state_frame()?
        {
            PreStateFrame::Diff(diff) => Ok(diff),
            _ => anyhow::bail!("expected a diff mode trace"),
        }
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.substrate_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for KitchensinkNode {
    fn drop(&mut self) {
        if let Some(mut child) = self.process_proxy.take() {
            let _ = child.kill();
        }
        if let Some(mut child) = self.process_substrate.take() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use revive_dt_config::Arguments;
    use temp_dir::TempDir;

    use super::KitchensinkNode;
    use crate::{GENESIS_JSON, Node};

    fn test_config() -> (Arguments, TempDir) {
        let mut config = Arguments::default();
        let temp_dir = TempDir::new().unwrap();

        config.working_directory = temp_dir.path().to_path_buf().into();

        config.kitchensink = PathBuf::from("substrate-node");
        config.eth_proxy = PathBuf::from("eth-rpc");

        (config, temp_dir)
    }

    #[test]
    fn spawn_works() {
        let (config, _temp_dir) = test_config();

        let mut node = KitchensinkNode::new(&config);
        node.spawn(GENESIS_JSON.to_string()).unwrap();
    }

    #[test]
    fn version_works() {
        let (config, _temp_dir) = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected substrate-node version string, got: {version}"
        );
    }

    #[test]
    fn eth_rpc_version_works() {
        let (config, _temp_dir) = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }
}
