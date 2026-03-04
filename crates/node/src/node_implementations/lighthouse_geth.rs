//! This module implements an Ethereum network that's identical to mainnet in all ways where Geth is
//! used as the execution engine and doesn't perform any kind of consensus, which now happens on the
//! beacon chain and between the beacon nodes. We're using lighthouse as the consensus node in this
//! case with 12 second block slots which is identical to Ethereum mainnet.
//!
//! Ths implementation uses the Kurtosis tool to spawn the various nodes that are needed which means
//! that this tool is now a requirement that needs to be installed in order for this target to be
//! used. Additionally, the Kurtosis tool uses Docker and therefore docker is a another dependency
//! that the tool has.

#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The go-ethereum node instance implementation.
///
/// Implements helpers to initialize, spawn and wait the node.
///
/// Assumes dev mode and IPC only (`P2P`, `http` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
#[allow(clippy::type_complexity)]
pub struct LighthouseGethNode {
    /* Node Identifier */
    id: u32,
    ws_connection_string: String,
    http_connection_string: String,
    enclave_name: String,

    /* Directory Paths */
    base_directory: PathBuf,
    logs_directory: PathBuf,

    /* File Paths */
    config_file_path: PathBuf,

    /* Binary Paths & Timeouts */
    kurtosis_binary_path: PathBuf,

    /* Spawned Processes */
    process: Option<Process>,

    /* Prefunded Account Information */
    prefunded_account_address: Address,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,

    persistent_http_provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    persistent_ws_provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,

    use_fallback_gas_filler: bool,
}

impl LighthouseGethNode {
    const BASE_DIRECTORY: &str = "lighthouse";
    const LOGS_DIRECTORY: &str = "logs";

    const CONFIG_FILE_NAME: &str = "config.yaml";

    const VALIDATOR_MNEMONIC: &str = "giant issue aisle success illegal bike spike question tent bar rely arctic volcano long crawl hungry vocal artwork sniff fantasy very lucky have athlete";

    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasKurtosisConfiguration
        + Clone,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let working_directory_configuration = context.as_working_directory_configuration();
        let wallet_configuration = context.as_wallet_configuration();
        let kurtosis_configuration = context.as_kurtosis_configuration();

        let geth_directory = working_directory_configuration
            .working_directory
            .as_path()
            .join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        let wallet = wallet_configuration.wallet();

        Self {
            /* Node Identifier */
            id,
            ws_connection_string: String::default(),
            http_connection_string: String::default(),
            enclave_name: format!(
                "enclave-{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Must not fail")
                    .as_nanos(),
                id
            ),

            /* File Paths */
            config_file_path: base_directory.join(Self::CONFIG_FILE_NAME),

            /* Directory Paths */
            logs_directory: base_directory.join(Self::LOGS_DIRECTORY),
            base_directory,

            /* Binary Paths & Timeouts */
            kurtosis_binary_path: kurtosis_configuration.path.clone(),

            /* Spawned Processes */
            process: None,

            /* Prefunded Account Information */
            prefunded_account_address: wallet.default_signer().address(),

            /* Provider Related Fields */
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            persistent_http_provider: Default::default(),
            persistent_ws_provider: Default::default(),
            use_fallback_gas_filler,
        }
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        self.init_directories()
            .context("Failed to initialize the directories of the Lighthouse Geth node.")?;
        self.init_kurtosis_config_file()
            .context("Failed to write the config file to the FS")?;

        Ok(self)
    }

    fn init_directories(&self) -> anyhow::Result<()> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for geth node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for geth node")?;

        Ok(())
    }

    fn init_kurtosis_config_file(&self) -> anyhow::Result<()> {
        let config = KurtosisNetworkConfig {
            participants: vec![ParticipantParameters {
                execution_layer_type: ExecutionLayerType::Geth,
                consensus_layer_type: ConsensusLayerType::Lighthouse,
                execution_layer_extra_parameters: vec![
                    "--syncmode=full".to_string(),
                    "--nodiscover".to_string(),
                    "--cache=4096".to_string(),
                    "--txlookuplimit=0".to_string(),
                    "--gcmode=archive".to_string(),
                    "--txpool.globalslots=500000".to_string(),
                    "--txpool.globalqueue=500000".to_string(),
                    "--txpool.accountslots=32768".to_string(),
                    "--txpool.accountqueue=32768".to_string(),
                    "--http.api=admin,engine,net,eth,web3,debug,txpool".to_string(),
                    "--http.addr=0.0.0.0".to_string(),
                    "--ws".to_string(),
                    "--ws.addr=0.0.0.0".to_string(),
                    "--ws.port=8546".to_string(),
                    "--ws.api=eth,net,web3,txpool,engine".to_string(),
                    "--ws.origins=*".to_string(),
                    "--miner.gaslimit=60000000".to_string(),
                    "--miner.gasprice=0".to_string(),
                ],
                consensus_layer_extra_parameters: vec!["--disable-quic".to_string()],
            }],
            network_parameters: NetworkParameters {
                preset: NetworkPreset::Mainnet,
                seconds_per_slot: 12,
                network_id: CHAIN_ID,
                deposit_contract_address: address!("0x00000000219ab540356cBB839Cbe05303d7705Fa"),
                altair_fork_epoch: 0,
                bellatrix_fork_epoch: 0,
                capella_fork_epoch: 0,
                deneb_fork_epoch: 0,
                electra_fork_epoch: 0,
                fulu_fork_epoch: u64::MAX,
                preregistered_validator_keys_mnemonic: Self::VALIDATOR_MNEMONIC.to_string(),
                num_validator_keys_per_node: 64,
                genesis_delay: 10,
                prefunded_accounts: {
                    let map = std::iter::once(self.prefunded_account_address)
                        .map(|address| (address, GenesisAccount::default().with_balance(U256::MAX)))
                        .collect::<BTreeMap<_, _>>();
                    serde_json::to_string(&map).unwrap()
                },
                gas_limit: 60_000_000,
                genesis_gaslimit: 60_000_000,
            },
            wait_for_finalization: false,
            port_publisher: Some(PortPublisherParameters {
                execution_layer_port_publisher_parameters: Some(
                    PortPublisherSingleItemParameters {
                        enabled: Some(true),
                        public_port_start: Some(32000 + self.id as u16 * 1000),
                    },
                ),
                consensus_layer_port_publisher_parameters: Some(
                    PortPublisherSingleItemParameters {
                        enabled: Some(true),
                        public_port_start: Some(59010 + self.id as u16 * 50),
                    },
                ),
            }),
        };

        let file = File::create(self.config_file_path.as_path())
            .context("Failed to open the config yaml file")?;
        serde_yaml_ng::to_writer(file, &config)
            .context("Failed to write the config to the yaml file")?;

        Ok(())
    }

    /// Spawn the go-ethereum node child process.
    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let process = Process::new(
            None,
            self.logs_directory.as_path(),
            self.kurtosis_binary_path.as_path(),
            |command, stdout, stderr| {
                command
                    .arg("run")
                    .arg("--enclave")
                    .arg(self.enclave_name.as_str())
                    .arg("github.com/ethpandaops/ethereum-package")
                    .arg("--args-file")
                    .arg(self.config_file_path.as_path())
                    .stdout(stdout)
                    .stderr(stderr);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(15 * 60),
                check_function: Box::new(|stdout, stderr| {
                    for line in [stdout, stderr].iter().flatten() {
                        if line.to_lowercase().contains("error encountered") {
                            anyhow::bail!("Encountered an error when starting Kurtosis")
                        } else if line.contains("RUNNING") {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }),
            },
        )
        .context("Failed to spawn the kurtosis enclave")
        .inspect_err(|err| {
            tracing::error!(?err, "Failed to spawn Kurtosis");
            self.shutdown().expect("Failed to shutdown kurtosis");
        })?;
        self.process = Some(process);

        let child = Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("inspect")
            .arg(self.enclave_name.as_str())
            .stdout(Stdio::piped())
            .spawn()
            .context("Failed to spawn the kurtosis enclave inspect process")?;

        let stdout = {
            let mut stdout = String::default();
            child
                .stdout
                .expect("Should be piped")
                .read_to_string(&mut stdout)
                .context("Failed to read stdout of kurtosis inspect to string")?;
            stdout
        };

        self.http_connection_string = stdout
            .split("el-1-geth-lighthouse")
            .nth(1)
            .and_then(|str| str.split(" rpc").nth(1))
            .and_then(|str| str.split("->").nth(1))
            .and_then(|str| str.split("\n").next())
            .and_then(|str| str.trim().split(" ").next())
            .map(|str| format!("http://{}", str.trim()))
            .context("Failed to find the HTTP connection string of Kurtosis")?;
        self.ws_connection_string = stdout
            .split("el-1-geth-lighthouse")
            .nth(1)
            .and_then(|str| str.split("ws").nth(1))
            .and_then(|str| str.split("->").nth(1))
            .and_then(|str| str.split("\n").next())
            .and_then(|str| str.trim().split(" ").next())
            .map(|str| format!("ws://{}", str.trim()))
            .context("Failed to find the WS connection string of Kurtosis")?;

        info!(
            http_connection_string = self.http_connection_string,
            ws_connection_string = self.ws_connection_string,
            "Discovered the connection strings for the node"
        );

        Ok(self)
    }

    #[instrument(
        level = "info",
        skip_all,
        fields(lighthouse_node_id = self.id, connection_string = self.ws_connection_string),
        err(Debug),
    )]
    #[allow(clippy::type_complexity)]
    async fn ws_provider(&self) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.persistent_ws_provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.ws_connection_string.as_str(),
                    FallbackGasFiller::default()
                        .with_fallback_mechanism(self.use_fallback_gas_filler),
                    ChainIdFiller::new(Some(CHAIN_ID)),
                    NonceFiller::new(self.nonce_manager.clone()),
                    self.wallet.clone(),
                )
                .await
                .context("Failed to construct the provider")
            })
            .await
            .cloned()
    }

    #[instrument(
        level = "info",
        skip_all,
        fields(lighthouse_node_id = self.id, connection_string = self.ws_connection_string),
        err(Debug),
    )]
    #[allow(clippy::type_complexity)]
    async fn http_provider(
        &self,
    ) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.persistent_http_provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.http_connection_string.as_str(),
                    FallbackGasFiller::default(),
                    ChainIdFiller::new(Some(CHAIN_ID)),
                    NonceFiller::new(self.nonce_manager.clone()),
                    self.wallet.clone(),
                )
                .await
                .context("Failed to construct the provider")
            })
            .await
            .cloned()
    }

    /// Funds all of the accounts in the Ethereum wallet from the initially funded account.
    fn fund_all_accounts(&self) -> FrameworkFuture<anyhow::Result<()>> {
        let provider = self.provider();
        let wallet = self.wallet.clone();
        let prefunded_account_address = self.prefunded_account_address;

        Box::pin(async move {
            let provider = provider.await.context("Failed to get provider")?;
            let mut full_block_subscriber = provider
                .subscribe_full_blocks()
                .into_stream()
                .await
                .context("Full block subscriber")?;

            let mut tx_hashes = futures::future::try_join_all(
                NetworkWallet::<Ethereum>::signer_addresses(wallet.as_ref())
                    .enumerate()
                    .map(move |(nonce, address)| {
                        let provider = provider.clone();
                        async move {
                            let mut transaction = TransactionRequest::default()
                                .from(prefunded_account_address)
                                .to(address)
                                .nonce(nonce as _)
                                .value(INITIAL_BALANCE.try_into().unwrap());
                            transaction.chain_id = Some(CHAIN_ID);
                            provider
                                .send_transaction(transaction)
                                .await
                                .map(|pending_transaction| *pending_transaction.tx_hash())
                        }
                    }),
            )
            .await
            .context("Failed to submit all transactions")?
            .into_iter()
            .collect::<HashSet<_>>();

            while let Some(block) = full_block_subscriber.next().await {
                let Ok(block) = block else {
                    continue;
                };

                let block_number = block.number();
                let block_timestamp = block.header.timestamp;
                let block_transaction_count = block.transactions.len();

                for hash in block.transactions.into_hashes().as_hashes().unwrap() {
                    tx_hashes.remove(hash);
                }

                info!(
                    block.number = block_number,
                    block.timestamp = block_timestamp,
                    block.transaction_count = block_transaction_count,
                    remaining_transactions = tx_hashes.len(),
                    "Discovered new block when funding accounts"
                );

                if tx_hashes.is_empty() {
                    break;
                }
            }

            Ok(())
        })
    }

    pub fn node_genesis(mut genesis: Genesis, wallet: &EthereumWallet) -> Genesis {
        for signer_address in NetworkWallet::<Ethereum>::signer_addresses(&wallet) {
            genesis
                .alloc
                .entry(signer_address)
                .or_insert(GenesisAccount::default().with_balance(U256::from(INITIAL_BALANCE)));
        }
        genesis
    }
}

impl NodeApi for LighthouseGethNode {
    fn pre_transactions(&mut self) -> FrameworkFuture<anyhow::Result<()>> {
        self.fund_all_accounts()
    }

    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.ws_connection_string
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn submit_transaction(
        &self,
        mut transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<alloy::primitives::TxHash>> {
        transaction.set_gas_price(10_000 * GWEI_TO_WEI as u128);
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .send_transaction(transaction)
                .await
                .context("Failed to submit transaction")
                .map(|pending_transaction| *pending_transaction.tx_hash())
        })
    }

    fn provider(&self) -> FrameworkFuture<anyhow::Result<DynProvider>> {
        let provider = self.persistent_ws_provider.clone();
        let connection_string = self.ws_connection_string.clone();
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
}

impl Node for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn shutdown(&mut self) -> anyhow::Result<()> {
        let mut child = Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("rm")
            .arg("-f")
            .arg(self.enclave_name.as_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn the enclave kill command");

        if !child
            .wait()
            .expect("Failed to wait for the enclave kill command")
            .success()
        {
            let stdout = {
                let mut stdout = String::default();
                child
                    .stdout
                    .take()
                    .expect("Should be piped")
                    .read_to_string(&mut stdout)
                    .context("Failed to read stdout of kurtosis inspect to string")?;
                stdout
            };
            let stderr = {
                let mut stderr = String::default();
                child
                    .stderr
                    .take()
                    .expect("Should be piped")
                    .read_to_string(&mut stderr)
                    .context("Failed to read stderr of kurtosis inspect to string")?;
                stderr
            };

            panic!(
                "Failed to shut down the enclave {} - stdout: {stdout}, stderr: {stderr}",
                self.enclave_name
            )
        }

        drop(self.process.take());

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.kurtosis_binary_path)
            .arg("version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn geth --version process")?
            .wait_with_output()
            .context("Failed to wait for geth --version output")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KurtosisNetworkConfig {
    pub participants: Vec<ParticipantParameters>,

    #[serde(rename = "network_params")]
    pub network_parameters: NetworkParameters,

    pub wait_for_finalization: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_publisher: Option<PortPublisherParameters>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParticipantParameters {
    #[serde(rename = "el_type")]
    pub execution_layer_type: ExecutionLayerType,

    #[serde(rename = "el_extra_params")]
    pub execution_layer_extra_parameters: Vec<String>,

    #[serde(rename = "cl_type")]
    pub consensus_layer_type: ConsensusLayerType,

    #[serde(rename = "cl_extra_params")]
    pub consensus_layer_extra_parameters: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExecutionLayerType {
    Geth,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ConsensusLayerType {
    Lighthouse,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct NetworkParameters {
    pub preset: NetworkPreset,

    pub seconds_per_slot: u64,

    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub network_id: u64,

    pub deposit_contract_address: Address,

    pub altair_fork_epoch: u64,
    pub bellatrix_fork_epoch: u64,
    pub capella_fork_epoch: u64,
    pub deneb_fork_epoch: u64,
    pub electra_fork_epoch: u64,
    pub fulu_fork_epoch: u64,

    pub preregistered_validator_keys_mnemonic: String,

    pub num_validator_keys_per_node: u64,

    pub genesis_delay: u64,
    pub genesis_gaslimit: u64,
    pub gas_limit: u64,

    pub prefunded_accounts: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NetworkPreset {
    Mainnet,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PortPublisherParameters {
    #[serde(rename = "el", skip_serializing_if = "Option::is_none")]
    pub execution_layer_port_publisher_parameters: Option<PortPublisherSingleItemParameters>,

    #[serde(rename = "cl", skip_serializing_if = "Option::is_none")]
    pub consensus_layer_port_publisher_parameters: Option<PortPublisherSingleItemParameters>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct PortPublisherSingleItemParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_port_start: Option<u16>,
}

/// Custom serializer/deserializer for u128 values as 0x-prefixed hex strings
pub struct HexPrefixedU128;

impl serde_with::SerializeAs<u128> for HexPrefixedU128 {
    fn serialize_as<S>(source: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_string = format!("0x{source:x}");
        serializer.serialize_str(&hex_string)
    }
}

impl<'de> serde_with::DeserializeAs<'de, u128> for HexPrefixedU128 {
    fn deserialize_as<D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_string = String::deserialize(deserializer)?;
        if let Some(hex_part) = hex_string.strip_prefix("0x") {
            u128::from_str_radix(hex_part, 16).map_err(serde::de::Error::custom)
        } else {
            Err(serde::de::Error::custom("Expected 0x-prefixed hex string"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn test_config() -> Test {
        let mut config = Test::default();
        config.wallet.additional_keys = 100;
        config
    }

    fn new_node() -> (Test, LighthouseGethNode) {
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
        let mut node = LighthouseGethNode::new(context.clone(), true);
        node.init(context.genesis.genesis().unwrap().clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    #[tokio::test]
    #[ignore = "Ignored since they take a long time to run"]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = new_node();
        node.fund_all_accounts().await.expect("Failed");

        let account_address = context.wallet.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let receipt = node.execute_transaction(transaction).await;

        // Assert
        let _ = receipt.expect("Failed to send the transfer transaction");
    }

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn version_works() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let version = node.version();

        // Assert
        let version = version.expect("Failed to get the version");
        assert!(
            version.starts_with("CLI Version"),
            "expected version string, got: '{version}'"
        );
    }
}
