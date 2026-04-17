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
    wrapper_directory: PathBuf,

    /* File Paths */
    config_file_path: PathBuf,

    /* Binary Paths & Timeouts */
    kurtosis_binary_path: PathBuf,

    /* Spawned Processes */
    process: Option<Process>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: ZeroedCachedNonceManager,

    persistent_ws_provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    persistent_ws_subscriptions_provider:
        Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,

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
            wrapper_directory: base_directory.join("wrapper"),
            base_directory,

            /* Binary Paths & Timeouts */
            kurtosis_binary_path: kurtosis_configuration.path.clone(),

            /* Spawned Processes */
            process: None,

            /* Provider Related Fields */
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            persistent_ws_provider: Default::default(),
            persistent_ws_subscriptions_provider: Default::default(),
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
        create_dir_all(&self.wrapper_directory)
            .context("Failed to create wrapper directory for geth node")?;

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
                    "--cache=4294967295".to_string(),
                    "--txlookuplimit=0".to_string(),
                    "--gcmode=archive".to_string(),
                    "--state.scheme=path".to_string(),
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
                    "--rpc.txfeecap=0".to_string(),
                    "--rpc.batch-request-limit=0".to_string(),
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
                prefunded_accounts: "{}".to_string(),
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

        // Write the kurtosis args config file.
        let file = File::create(self.config_file_path.as_path())
            .context("Failed to open the config yaml file")?;
        serde_yaml_ng::to_writer(file, &config)
            .context("Failed to write the config to the yaml file")?;

        // Write the prefunded accounts JSON into the wrapper package. This is
        // read server-side by Starlark's `read_file` and injected into the
        // ethereum-package via `additional_preloaded_contracts`, bypassing the
        // 4 MB gRPC arg size limit that `prefunded_accounts` is subject to.
        let prefunded_accounts = {
            let map = NetworkWallet::<Ethereum>::signer_addresses(self.wallet.as_ref())
                .map(|address| (address, GenesisAccount::default().with_balance(U256::MAX)))
                .collect::<BTreeMap<_, _>>();
            serde_json::to_string(&map).context("Failed to serialize prefunded accounts")?
        };
        std::fs::write(
            self.wrapper_directory.join("prefunded_accounts.json"),
            &prefunded_accounts,
        )
        .context("Failed to write prefunded_accounts.json")?;

        // Write the Kurtosis package manifest for the wrapper.
        std::fs::write(
            self.wrapper_directory.join("kurtosis.yml"),
            "name: github.com/local/ethereum-wrapper\nreplace:\n  {}\n",
        )
        .context("Failed to write kurtosis.yml")?;

        // Write the Starlark entrypoint that reads the prefunded accounts from
        // the package files and passes them to the upstream ethereum-package.
        std::fs::write(
            self.wrapper_directory.join("main.star"),
            r#"ethereum_package = import_module("github.com/ethpandaops/ethereum-package/main.star")

def run(plan, args={}):
    accounts_json = read_file("./prefunded_accounts.json")
    accounts = json.decode(accounts_json)
    if "network_params" not in args:
        args["network_params"] = {}
    args["network_params"]["additional_preloaded_contracts"] = accounts
    return ethereum_package.run(plan, args)
"#,
        )
        .context("Failed to write main.star")?;

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
                    .arg(self.wrapper_directory.as_path())
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
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.ws_connection_string
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
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

    fn subscriptions_provider(&self) -> FrameworkFuture<anyhow::Result<DynProvider>> {
        let provider = self.persistent_ws_subscriptions_provider.clone();
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

        let _ = remove_dir_all(self.wrapper_directory.as_path());

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
