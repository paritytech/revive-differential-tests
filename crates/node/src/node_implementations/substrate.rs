#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The number of blocks that should be cached by the revive-dev-node and the eth-rpc.
const NUMBER_OF_CACHED_BLOCKS: u32 = 100_000;

/// A node implementation for Substrate based chains. Currently, this supports either substrate
/// or the revive-dev-node which is done by changing the path and some of the other arguments passed
/// to the command.
#[derive(Debug)]

pub struct SubstrateNode {
    id: u32,
    node_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    export_chainspec_command: String,
    rpc_url: String,
    base_directory: PathBuf,
    logs_directory: PathBuf,
    substrate_process: Option<Process>,
    eth_proxy_process: Option<Process>,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,
    consensus: Option<String>,
    use_fallback_gas_filler: bool,
    node_logging_level: String,
    eth_rpc_logging_level: String,
}

impl SubstrateNode {
    const BASE_DIRECTORY: &str = "substrate";
    const LOGS_DIRECTORY: &str = "logs";
    const DATA_DIRECTORY: &str = "chains";

    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    pub const REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND: &str = "build-spec";

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_path: PathBuf,
        export_chainspec_command: &str,
        consensus: Option<String>,
        context: impl HasWorkingDirectoryConfiguration + HasEthRpcConfiguration + HasWalletConfiguration,
        existing_connection_strings: &[String],
        use_fallback_gas_filler: bool,
        node_logging_level: String,
        eth_rpc_logging_level: String,
    ) -> Self {
        let working_directory_path = context
            .as_working_directory_configuration()
            .working_directory
            .as_path();
        let eth_rpc_path = context.as_eth_rpc_configuration().path.as_path();
        let wallet = context.as_wallet_configuration().wallet();

        let substrate_directory = working_directory_path.join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = substrate_directory.join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);

        let rpc_url = existing_connection_strings
            .get(id as usize)
            .cloned()
            .unwrap_or_default();

        Self {
            id,
            node_binary: node_path,
            eth_proxy_binary: eth_rpc_path.to_path_buf(),
            export_chainspec_command: export_chainspec_command.to_string(),
            rpc_url,
            base_directory,
            logs_directory,
            substrate_process: None,
            eth_proxy_process: None,
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
            substrate_provider: Default::default(),
            consensus,
            use_fallback_gas_filler,
            node_logging_level,
            eth_rpc_logging_level,
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        static CHAINSPEC_MUTEX: StdMutex<Option<Value>> = StdMutex::new(None);

        if !self.rpc_url.is_empty() {
            return Ok(self);
        }

        trace!("Removing the various directories");
        let _ = remove_dir_all(self.base_directory.as_path());
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        trace!("Creating the various directories");
        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for substrate node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for substrate node")?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        trace!("Creating the node genesis");
        let chainspec_json = {
            let mut chainspec_mutex = CHAINSPEC_MUTEX.lock().expect("Poisoned");
            match chainspec_mutex.as_ref() {
                Some(chainspec_json) => chainspec_json.clone(),
                None => {
                    let chainspec_json = Self::node_genesis(
                        &self.node_binary,
                        &self.export_chainspec_command,
                        &self.wallet,
                    )
                    .context("Failed to prepare the chainspec command")?;
                    *chainspec_mutex = Some(chainspec_json.clone());
                    chainspec_json
                }
            }
        };

        trace!("Writing the node genesis");
        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)
                .context("Failed to create substrate template chainspec file")?,
            &chainspec_json,
        )
        .context("Failed to write substrate template chainspec JSON")?;
        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        if !self.rpc_url.is_empty() {
            return Ok(());
        }

        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        let chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        trace!("Spawning the substrate process");
        let substrate_process = Process::new(
            "node",
            self.logs_directory.as_path(),
            self.node_binary.as_path(),
            |command, stdout_file, stderr_file| {
                let cmd = command
                    .arg("--dev")
                    .arg("--chain")
                    .arg(chainspec_path)
                    .arg("--base-path")
                    .arg(&self.base_directory)
                    .arg("--rpc-port")
                    .arg(substrate_rpc_port.to_string())
                    .arg("--name")
                    .arg(format!("revive-substrate-{}", self.id))
                    .arg("--force-authoring")
                    .arg("--rpc-methods")
                    .arg("Unsafe")
                    .arg("--rpc-cors")
                    .arg("all")
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--pool-limit")
                    .arg(u32::MAX.to_string())
                    .arg("--pool-kbytes")
                    .arg(u32::MAX.to_string())
                    .arg("--state-pruning")
                    .arg(NUMBER_OF_CACHED_BLOCKS.to_string())
                    .env("RUST_LOG", self.node_logging_level.as_str())
                    .stdout(stdout_file)
                    .stderr(stderr_file);
                if let Some(consensus) = self.consensus.as_ref() {
                    cmd.arg("--consensus").arg(consensus.clone());
                }
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(90),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::SUBSTRATE_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );
        match substrate_process {
            Ok(process) => self.substrate_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start substrate, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after substrate start error")?;
                return Err(err);
            }
        }

        trace!("Spawning eth-rpc process");
        let eth_proxy_process = Process::new(
            "proxy",
            self.logs_directory.as_path(),
            self.eth_proxy_binary.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--rpc-port")
                    .arg(proxy_rpc_port.to_string())
                    .arg("--node-rpc-url")
                    .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--rpc-max-batch-request-len")
                    .arg(u32::MAX.to_string())
                    .env("RUST_LOG", self.eth_rpc_logging_level.as_str())
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(30),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::ETH_PROXY_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );
        match eth_proxy_process {
            Ok(process) => self.eth_proxy_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start eth proxy, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth proxy start error")?;
                return Err(err);
            }
        }

        Ok(())
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

    async fn provider(&self) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.rpc_url.as_str(),
                    FallbackGasFiller::default()
                        .with_fallback_mechanism(self.use_fallback_gas_filler),
                    ChainIdFiller::default(),
                    NonceFiller::new(self.nonce_manager.clone()),
                    self.wallet.clone(),
                )
                .await
                .context("Failed to construct the provider")
            })
            .await
            .cloned()
    }

    pub fn node_genesis(
        node_path: &Path,
        export_chainspec_command: &str,
        wallet: &EthereumWallet,
    ) -> anyhow::Result<serde_json::Value> {
        trace!("Exporting the chainspec");
        let output = Command::new(node_path)
            .arg(export_chainspec_command)
            .arg("--chain")
            .arg("dev")
            .env_remove("RUST_LOG")
            .output()
            .context("Failed to export the chain-spec")?;

        trace!("Waiting for chainspec export");
        if !output.status.success() {
            anyhow::bail!(
                "substrate-node export-chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        trace!("Obtained chainspec");
        let content = String::from_utf8(output.stdout)
            .context("Failed to decode Substrate export-chain-spec output as UTF-8")?;
        let mut chainspec_json = serde_json::from_str::<serde_json::Value>(&content)
            .context("Failed to parse Substrate chain spec JSON")?;

        trace!("Adding addresses to chainspec");
        inject_wallet_balances(&mut chainspec_json, wallet)?;

        Ok(chainspec_json)
    }
}

impl NodeApi for SubstrateNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.rpc_url
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn provider(&self) -> revive_dt_common::futures::FrameworkFuture<anyhow::Result<DynProvider>> {
        let provider = self.provider.clone();
        let connection_string = self.rpc_url.clone();
        let gas_filler =
            FallbackGasFiller::default().with_fallback_mechanism(self.use_fallback_gas_filler);
        let nonce_filler = NonceFiller::new(self.nonce_manager.clone());
        let wallet = self.wallet.clone();

        Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    construct_concurrency_limited_provider::<Ethereum, _>(
                        &connection_string,
                        gas_filler,
                        ChainIdFiller::default(),
                        nonce_filler,
                        wallet,
                    )
                    .await
                    .context("Failed to construct the provider")
                })
                .await
                .map(|provider| provider.clone().erased())
        })
    }

    fn substrate_provider(
        &self,
    ) -> Option<
        revive_dt_common::futures::FrameworkFuture<anyhow::Result<OnlineClient<PolkadotConfig>>>,
    > {
        let provider = self.substrate_provider.clone();
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let connection_string = format!("ws://127.0.0.1:{substrate_rpc_port}");

        Some(Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    OnlineClient::from_url(connection_string)
                        .await
                        .context("Failed to create a new online client")
                })
                .await
                .cloned()
        }))
    }
}

impl Node for SubstrateNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        drop(self.substrate_process.take());
        drop(self.eth_proxy_process.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY));

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.node_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn substrate --version")?
            .wait_with_output()
            .context("Failed to wait for substrate --version")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for SubstrateNode {
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;
    use std::sync::{LazyLock, Mutex};

    use std::fs;

    use alloy::primitives::U256;

    use super::*;
    use crate::Node;

    fn test_config() -> Test {
        Test::default()
    }

    fn new_node() -> (Test, SubstrateNode) {
        // Note: When we run the tests in the CI we found that if they're all
        // run in parallel then the CI is unable to start all of the nodes in
        // time and their start up times-out. Therefore, we want all of the
        // nodes to be started in series and not in parallel. To do this, we use
        // a dummy mutex here such that there can only be a single node being
        // started up at any point of time. This will make our tests run slower
        // but it will allow the node startup to not timeout.
        //
        // Note: an alternative to starting all of the nodes in series and not
        // in parallel would be for us to reuse the same node between tests
        // which is not the best thing to do in my opinion as it removes all
        // of the isolation between tests and makes them depend on what other
        // tests do. For example, if one test checks what the block number is
        // and another test submits a transaction then the tx test would have
        // side effects that affect the block number test.
        static NODE_START_MUTEX: Mutex<()> = Mutex::new(());
        let _guard = NODE_START_MUTEX.lock().unwrap();

        let context = test_config();
        let revive_dev_node_path = context.revive_dev_node.path.clone();
        let genesis = context.genesis.genesis().unwrap().clone();
        let mut node = SubstrateNode::new(
            revive_dev_node_path,
            SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND,
            None,
            context.clone(),
            &[],
            true,
            "".to_string(),
            "".to_string(),
        );
        node.init(genesis)
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    fn shared_state() -> &'static (Test, SubstrateNode) {
        static STATE: LazyLock<(Test, SubstrateNode)> = LazyLock::new(new_node);
        &STATE
    }

    fn shared_node() -> &'static SubstrateNode {
        &shared_state().1
    }

    #[tokio::test]
    #[ignore = "Ignored since it takes a long time to run"]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = shared_state();

        let provider = node.provider().await.expect("Failed to create provider");

        let account_address = context.wallet.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let mut pending_transaction = provider
            .send_transaction(transaction)
            .await
            .expect("Submission failed");
        pending_transaction.set_timeout(Some(Duration::from_secs(60)));

        // Assert
        let _ = pending_transaction
            .get_receipt()
            .await
            .expect("Failed to get the receipt for the transfer");
    }

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn test_init_generates_chainspec_with_balances() {
        let genesis_content = r#"
        {
            "alloc": {
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1": {
                    "balance": "1000000000000000000"
                },
                "Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2": {
                    "balance": "2000000000000000000"
                }
            }
        }
        "#;

        let context = test_config();
        let revive_dev_node_path = context.revive_dev_node.path.clone();
        let mut dummy_node = SubstrateNode::new(
            revive_dev_node_path,
            SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND,
            None,
            context,
            &[],
            true,
            "".to_string(),
            "".to_string(),
        );

        // Call `init()`
        dummy_node
            .init(serde_json::from_str(genesis_content).unwrap())
            .expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = dummy_node
            .base_directory
            .join(SubstrateNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents = fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Substrate addresses derived from the Ethereum addresses are in the file
        let first_eth_addr = crate::helpers::eth_to_polkadot_address(
            &"90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap(),
        );
        let second_eth_addr = crate::helpers::eth_to_polkadot_address(
            &"Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2".parse().unwrap(),
        );

        assert!(
            contents.contains(&first_eth_addr),
            "Chainspec should contain Substrate address for first Ethereum account"
        );
        assert!(
            contents.contains(&second_eth_addr),
            "Chainspec should contain Substrate address for second Ethereum account"
        );
    }

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn version_works() {
        let node = shared_node();

        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected substrate-node version string, got: {version}"
        );
    }

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn eth_rpc_version_works() {
        let node = shared_node();

        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }
}
