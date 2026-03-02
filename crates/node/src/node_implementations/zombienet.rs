//! # ZombieNode Implementation
//!
//! ## Required Binaries
//! This module requires the following binaries to be compiled and available in your PATH:
//!
//! 1. **polkadot-parachain**:
//!    ```bash
//!    git clone https://github.com/paritytech/polkadot-sdk.git
//!    cd polkadot-sdk
//!    cargo build --release --locked -p polkadot-parachain-bin --bin polkadot-parachain
//!    ```
//!
//! 2. **eth-rpc** (Revive EVM RPC server):
//!    ```bash
//!    git clone https://github.com/paritytech/polkadot-sdk.git
//!    cd polkadot-sdk
//!    cargo build --locked --profile production -p pallet-revive-eth-rpc --bin eth-rpc
//!    ```
//!
//! 3. **polkadot** (for the relay chain):
//!    ```bash
//!    # In polkadot-sdk directory
//!    cargo build --locked --profile testnet --features fast-runtime --bin polkadot --bin polkadot-prepare-worker --bin polkadot-execute-worker
//!    ```
//!
//! Make sure to add the build output directories to your PATH or provide
//! the full paths in your configuration.

#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A Zombienet network where collator is `polkadot-parachain` node with `eth-rpc` [`ZombieNode`]
/// abstracts away the details of managing the zombienet network and provides an interface to
/// interact with the parachain's Ethereum RPC.
#[derive(Debug, Default)]
pub struct ZombienetNode {
    /* Node Identifier */
    id: u32,
    connection_string: String,
    collator_ws_uri: Option<String>,

    /* Directory Paths */
    base_directory: PathBuf,
    logs_directory: PathBuf,

    /* Binary Paths & Timeouts */
    eth_proxy_binary: PathBuf,
    polkadot_parachain_path: PathBuf,

    /* Spawned Processes */
    eth_rpc_process: Option<Process>,

    /* Zombienet Network */
    config_path: Option<PathBuf>,
    network_config: Option<zombienet_sdk::NetworkConfig>,
    network: Option<zombienet_sdk::Network<LocalFileSystem>>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,

    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<SubstrateConfig>>>,

    use_fallback_gas_filler: bool,

    eth_rpc_logging_level: String,

    /// The tokio runtime that the zombienet SDK tasks run on. Must be kept alive
    /// so that the log-reading tasks continue draining the process output pipes.
    /// Without this, the child processes block on stderr writes when the pipe buffer fills.
    zombienet_runtime: Option<tokio::runtime::Runtime>,
}

impl ZombienetNode {
    const BASE_DIRECTORY: &str = "zombienet";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const ETH_RPC_BASE_PORT: u16 = 8545;

    const ETH_RPC_READY_MARKER: &str = "Running JSON-RPC server";

    const EXPORT_CHAINSPEC_COMMAND: &str = "build-spec";

    pub fn new(
        polkadot_parachain_path: PathBuf,
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasZombienetConfiguration,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let eth_proxy_binary = context.as_eth_rpc_configuration().path.to_owned();
        let working_directory_path = context.as_working_directory_configuration();
        let zombienet_configuration = context.as_zombienet_configuration();
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path
            .working_directory
            .join(Self::BASE_DIRECTORY)
            .join(id.to_string());
        let base_directory = base_directory.canonicalize().unwrap_or(base_directory);
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);
        let wallet = context.as_wallet_configuration().wallet();

        Self {
            id,
            base_directory,
            logs_directory,
            wallet,
            polkadot_parachain_path,
            eth_proxy_binary,
            nonce_manager: CachedNonceManager::default(),
            config_path: zombienet_configuration.config_path.clone(),
            network_config: None,
            network: None,
            eth_rpc_process: None,
            connection_string: String::new(),
            collator_ws_uri: None,
            provider: Default::default(),
            substrate_provider: Default::default(),
            use_fallback_gas_filler,
            eth_rpc_logging_level: context.as_eth_rpc_configuration().logging_level.clone(),
            zombienet_runtime: None,
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for zombie node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for zombie node")?;

        let config_path = self.config_path.as_ref().context(
            "A zombienet config file path is required. Provide one via --zombienet.config-path",
        )?;

        let toml_content =
            std::fs::read_to_string(config_path).context("Failed to read zombienet config file")?;
        let mut toml_value: toml::Value =
            toml::from_str(&toml_content).context("Failed to parse zombienet TOML")?;

        self.inject_prefunded_chainspec(&mut toml_value)?;

        let modified_toml_path = self.base_directory.join("zombienet.toml");
        std::fs::write(
            &modified_toml_path,
            toml::to_string(&toml_value).context("Failed to serialize modified TOML")?,
        )
        .context("Failed to write modified zombienet config")?;

        let network_config = NetworkConfig::load_from_toml(
            modified_toml_path
                .to_str()
                .context("Invalid modified TOML path")?,
        )
        .context("Failed to load zombienet config")?;

        self.network_config = Some(network_config);

        Ok(self)
    }

    /// Runs the parachain's `chain_spec_command`, appends wallet balances to the generated
    /// chainspec, writes the result to a file, and replaces `chain_spec_command` with
    /// `chain_spec_path` in the TOML config.
    fn inject_prefunded_chainspec(&self, toml_value: &mut toml::Value) -> anyhow::Result<()> {
        let parachain = toml_value
            .get_mut("parachains")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.first_mut())
            .context("No [[parachains]] found in zombienet config")?;

        let chain_name = parachain
            .get("chain")
            .and_then(|v| v.as_str())
            .unwrap_or("asset-hub-westend-local");

        let command_template = parachain
            .get("chain_spec_command")
            .and_then(|v| v.as_str())
            .context("No chain_spec_command found in the parachain config")?;
        let command = command_template.replace("{{chainName}}", chain_name);

        // Run the chain_spec_command to get the base chainspec.
        let output = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env_remove("RUST_LOG")
            .output()
            .context("Failed to run chain_spec_command")?;

        if !output.status.success() {
            anyhow::bail!(
                "chain_spec_command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)
            .context("Failed to decode chain_spec_command output as UTF-8")?;
        let mut chainspec = serde_json::from_str::<serde_json::Value>(&content)
            .context("Failed to parse chainspec JSON")?;

        // Append wallet balances to the existing balances array.
        let balances = chainspec["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
            .as_array_mut()
            .context("Failed to find balances array in chainspec")?;

        for address in NetworkWallet::<Ethereum>::signer_addresses(&self.wallet) {
            let substrate_address = Self::eth_to_polkadot_address(&address);
            balances.push(json!((substrate_address, INITIAL_BALANCE)));
        }

        // Write the pre-funded chainspec to a file.
        let chainspec_path = self.base_directory.join("chainspec.json");
        std::fs::write(
            &chainspec_path,
            serde_json::to_string_pretty(&chainspec).context("Failed to serialize chainspec")?,
        )
        .context("Failed to write chainspec")?;

        tracing::info!(
            path = %chainspec_path.display(),
            "Generated pre-funded chainspec"
        );

        // Swap chain_spec_command → chain_spec_path in the TOML.
        let table = parachain
            .as_table_mut()
            .context("Parachain config is not a table")?;
        table.remove("chain_spec_command");
        table.insert(
            "chain_spec_path".to_string(),
            toml::Value::String(chainspec_path.to_string_lossy().into_owned()),
        );

        Ok(())
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let network_config = self
            .network_config
            .clone()
            .context("Node not initialized, call init() first")?;

        // TODO: Look into the possibility of removing this in the future, perhaps by reintroducing
        // the blocking runtime abstraction and making it available to the entire program so that we
        // don't need to be spawning multiple different runtimes.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let network = rt.block_on(async {
            network_config
                .spawn_native()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to spawn zombienet network: {e:?}"))
        })?;

        tracing::debug!("Zombienet network is up");

        let collator = network
            .parachains()
            .first()
            .context("No parachains found in the spawned zombienet network")?
            .collators()
            .into_iter()
            .next()
            .context("No collators found in the first parachain")?;
        let collator_ws_uri = collator.ws_uri().to_string();
        tracing::info!(
            collator_ws_uri,
            collator_name = collator.name(),
            "Using collator"
        );

        let eth_rpc_port = Self::ETH_RPC_BASE_PORT + self.id as u16;
        let node_rpc_url = collator_ws_uri.clone();

        let eth_rpc_process = Process::new(
            "proxy",
            self.logs_directory.as_path(),
            self.eth_proxy_binary.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--node-rpc-url")
                    .arg(node_rpc_url)
                    .arg("--rpc-cors")
                    .arg("all")
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--rpc-port")
                    .arg(eth_rpc_port.to_string())
                    .arg("--index-last-n-blocks")
                    .arg(100_000u32.to_string())
                    .arg("--cache-size")
                    .arg(100_000u32.to_string())
                    .env("RUST_LOG", self.eth_rpc_logging_level.as_str())
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(30),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::ETH_RPC_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );

        match eth_rpc_process {
            Ok(process) => self.eth_rpc_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start eth proxy, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth proxy start error")?;
                return Err(err);
            }
        }

        tracing::debug!("eth-rpc is up");

        self.connection_string = format!("http://localhost:{}", eth_rpc_port);
        self.collator_ws_uri = Some(collator_ws_uri);
        self.network = Some(network);
        self.zombienet_runtime = Some(rt);

        // Wait for the parachain to start producing blocks. After zombienet spawns, the parachain
        // needs to be onboarded through the relay chain which takes several epochs. Without this
        // wait, transactions submitted immediately would time out.
        self.wait_for_first_block()?;

        Ok(())
    }

    fn wait_for_first_block(&self) -> anyhow::Result<()> {
        tracing::info!("Waiting for parachain to produce first block...");

        let connection_string = self.connection_string.clone();
        let rt = self
            .zombienet_runtime
            .as_ref()
            .context("Zombienet runtime not available")?;
        rt.block_on(async {
            let provider = alloy::providers::ProviderBuilder::new().connect_http(
                connection_string
                    .parse()
                    .context("Invalid connection string")?,
            );

            let timeout = Duration::from_secs(300);
            let start = std::time::Instant::now();
            let poll_interval = Duration::from_secs(5);

            loop {
                if start.elapsed() > timeout {
                    anyhow::bail!(
                        "Timed out waiting for parachain to produce blocks after {}s",
                        timeout.as_secs()
                    );
                }

                match provider.get_block_number().await {
                    Ok(block_number) if block_number > 0 => {
                        tracing::info!(
                            block_number,
                            elapsed_secs = start.elapsed().as_secs(),
                            "Parachain is producing blocks"
                        );
                        return Ok(());
                    }
                    _ => {
                        tokio::time::sleep(poll_interval).await;
                    }
                }
            }
        })
    }

    fn eth_to_polkadot_address(address: &Address) -> String {
        let eth_bytes = address.0.0;

        let mut padded = [0xEEu8; 32];
        padded[..20].copy_from_slice(&eth_bytes);

        let account_id = AccountId32::from(padded);
        account_id.to_ss58check()
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
                    self.connection_string.as_str(),
                    FallbackGasFiller::default()
                        .with_fallback_mechanism(self.use_fallback_gas_filler),
                    ChainIdFiller::default(), // TODO: use CHAIN_ID constant
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
        wallet: &EthereumWallet,
    ) -> anyhow::Result<serde_json::Value> {
        let output = Command::new(node_path)
            .arg(Self::EXPORT_CHAINSPEC_COMMAND)
            .arg("--chain")
            .arg("asset-hub-westend-local")
            .env_remove("RUST_LOG")
            .output()
            .context("Failed to export the chainspec of the chain")?;

        if !output.status.success() {
            anyhow::bail!(
                "substrate-node export-chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)
            .context("Failed to decode Substrate export-chain-spec output as UTF-8")?;
        let mut chainspec_json = serde_json::from_str::<serde_json::Value>(&content)
            .context("Failed to parse Substrate chain spec JSON")?;

        let existing_chainspec_balances =
            chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
                .as_array_mut()
                .expect("Can't fail");

        for address in NetworkWallet::<Ethereum>::signer_addresses(wallet) {
            let substrate_address = Self::eth_to_polkadot_address(&address);
            let balance = INITIAL_BALANCE;
            existing_chainspec_balances.push(json!((substrate_address, balance)));
        }

        Ok(chainspec_json)
    }
}

impl NodeApi for ZombienetNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.connection_string
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn provider(&self) -> revive_dt_common::futures::FrameworkFuture<anyhow::Result<DynProvider>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string.clone();
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
    ) -> Option<revive_dt_common::futures::FrameworkFuture<anyhow::Result<OnlineClient<SubstrateConfig>>>>
    {
        let provider = self.substrate_provider.clone();
        let connection_string = self.collator_ws_uri.clone().unwrap_or_default();
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

impl Node for ZombienetNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        // Kill the eth_rpc process
        drop(self.eth_rpc_process.take());

        // Destroy the network on a dedicated thread to avoid "Cannot start a runtime from
        // within a runtime" panics when drop is called from a tokio async context.
        let _ = self
            .zombienet_runtime
            .take()
            .zip(self.network.take())
            .map(|(runtime, network)| {
                let jh = std::thread::spawn(move || {
                    runtime.block_on(async {
                        if let Err(e) = network.destroy().await {
                            tracing::warn!("Failed to destroy zombienet network: {e:?}");
                        }
                    })
                });
                let _ = jh.join();
            });

        // Remove the database directory
        if let Err(e) = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY)) {
            tracing::warn!("Failed to remove database directory: {e:?}");
        }

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.polkadot_parachain_path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed execute --version")?
            .wait_with_output()
            .context("Failed to wait --version")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for ZombienetNode {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use alloy::{primitives::U256, rpc::types::TransactionRequest};

    use super::*;

    mod utils {
        use super::*;

        use std::sync::Arc;
        use tokio::sync::OnceCell;

        pub fn test_config() -> Test {
            Test::default()
        }

        pub async fn new_node() -> (Test, ZombienetNode) {
            let context = test_config();
            let parachain_path = context.polkadot_parachain.path.clone();
            let genesis = context.genesis.genesis().unwrap().clone();
            let mut node = ZombienetNode::new(parachain_path, context.clone(), true);
            node.init(genesis).unwrap();

            // Run spawn_process in a blocking thread
            let node = tokio::task::spawn_blocking(move || {
                node.spawn_process().unwrap();
                node
            })
            .await
            .expect("Failed to spawn process");

            (context, node)
        }

        pub async fn shared_state() -> &'static (Test, Arc<ZombienetNode>) {
            static NODE: OnceCell<(Test, Arc<ZombienetNode>)> = OnceCell::const_new();

            NODE.get_or_init(|| async {
                let (context, node) = new_node().await;
                (context, Arc::new(node))
            })
            .await
        }

        pub async fn shared_node() -> &'static Arc<ZombienetNode> {
            &shared_state().await.1
        }
    }
    use utils::{new_node, test_config};

    #[tokio::test]
    #[ignore = "Ignored since CI doesn't have zombienet installed"]
    async fn test_transfer_transaction_should_return_receipt() {
        // Arrange
        let (ctx, node) = new_node().await;

        let provider = node.provider().await.expect("Failed to create provider");
        let account_address = ctx.wallet.wallet().default_signer().address();
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
    #[ignore = "Ignored since CI doesn't have zombienet installed"]
    fn print_eth_to_polkadot_mappings() {
        let eth_addresses = vec![
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
            "0xffffffffffffffffffffffffffffffffffffffff",
            "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
        ];

        for eth_addr in eth_addresses {
            let ss58 = ZombienetNode::eth_to_polkadot_address(&eth_addr.parse().unwrap());

            println!("Ethereum: {eth_addr} -> Polkadot SS58: {ss58}");
        }
    }

    #[test]
    #[ignore = "Ignored since CI doesn't have zombienet installed"]
    fn test_eth_to_polkadot_address() {
        let cases = vec![
            (
                "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "0x0000000000000000000000000000000000000000",
                "5C4hrfjw9DjXZTzV3MwzrrAr9P1MLDHajjSidz9bR544LEq1",
            ),
            (
                "0xffffffffffffffffffffffffffffffffffffffff",
                "5HrN7fHLXWcFiXPwwtq2EkSGns9eMmoUQnbVKweNz3VVr6N4",
            ),
        ];

        for (eth_addr, expected_ss58) in cases {
            let result = ZombienetNode::eth_to_polkadot_address(&eth_addr.parse().unwrap());
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    #[ignore = "Ignored since CI doesn't have zombienet installed"]
    fn eth_rpc_version_works() {
        // Arrange
        let context = test_config();
        let node = ZombienetNode::new(context.polkadot_parachain.path.clone(), context, true);

        // Act
        let version = node.eth_rpc_version().unwrap();

        // Assert
        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }

    #[test]
    #[ignore = "Ignored since CI doesn't have zombienet installed"]
    fn version_works() {
        // Arrange
        let context = test_config();
        let node = ZombienetNode::new(context.polkadot_parachain.path.clone(), context, true);

        // Act
        let version = node.version().unwrap();

        // Assert
        assert!(
            version.starts_with("polkadot-parachain"),
            "Expected Polkadot-parachain version string, got: {version}"
        );
    }
}
