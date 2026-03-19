#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The number of blocks that should be cached by the polkadot-omni-node and the eth-rpc.
const NUMBER_OF_CACHED_BLOCKS: u32 = 100_000;

/// A node implementation for the polkadot-omni-node.
#[derive(Debug)]

pub struct PolkadotOmnichainNode {
    /// The id of the node.
    id: u32,

    /// The path of the polkadot-omni-chain node binary.
    polkadot_omnichain_node_binary_path: PathBuf,
    /// The path of the eth-rpc binary.
    eth_rpc_binary_path: PathBuf,
    /// The path of the runtime's WASM that this node will be spawned with.
    chain_spec_path: Option<PathBuf>,
    /// The path of the base directory which contains all of the stored data for this node.
    base_directory_path: PathBuf,
    /// The path of the logs directory which contains all of the stored logs.
    logs_directory_path: PathBuf,

    /// Defines the amount of time to wait before considering that the node start has timed out.
    node_start_timeout: Duration,

    /// The id of the parachain that this node will be spawning.
    parachain_id: Option<usize>,
    /// The block time.
    block_time: Duration,

    /// The node's process.
    polkadot_omnichain_node_process: Option<Process>,
    /// The eth-rpc's process.
    eth_rpc_process: Option<Process>,

    /// The URL of the eth-rpc.
    rpc_url: String,
    /// The wallet object that's used to sign any transaction submitted through this node.
    wallet: Arc<EthereumWallet>,
    /// The nonce manager used to populate nonces for all transactions submitted through this node.
    nonce_manager: CachedNonceManager,
    /// The provider used for all RPC interactions with the RPC of this node.
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    /// The provider used for the substrate rpc.
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,

    /// A boolean that controls if the fallback gas filler should be used or not.
    use_fallback_gas_filler: bool,

    node_logging_level: String,
    eth_rpc_logging_level: String,
}

impl PolkadotOmnichainNode {
    const BASE_DIRECTORY: &str = "polkadot-omni-node";
    const LOGS_DIRECTORY: &str = "logs";

    const POLKADOT_OMNICHAIN_NODE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_RPC_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT: u16 = 9944;
    const BASE_ETH_RPC_PORT: u16 = 8545;

    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasPolkadotOmnichainNodeConfiguration,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let polkadot_omnichain_node_configuration =
            context.as_polkadot_omnichain_node_configuration();
        let working_directory_path = context
            .as_working_directory_configuration()
            .working_directory
            .as_path();
        let eth_rpc_path = context.as_eth_rpc_configuration().path.as_path();
        let wallet = context.as_wallet_configuration().wallet();

        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path
            .join(Self::BASE_DIRECTORY)
            .join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);

        Self {
            id,
            polkadot_omnichain_node_binary_path: polkadot_omnichain_node_configuration
                .path
                .to_path_buf(),
            eth_rpc_binary_path: eth_rpc_path.to_path_buf(),
            chain_spec_path: polkadot_omnichain_node_configuration
                .chain_spec_path
                .clone(),
            base_directory_path: base_directory,
            logs_directory_path: logs_directory,
            parachain_id: polkadot_omnichain_node_configuration.parachain_id,
            block_time: polkadot_omnichain_node_configuration.block_time_ms,
            polkadot_omnichain_node_process: Default::default(),
            eth_rpc_process: Default::default(),
            rpc_url: Default::default(),
            wallet,
            nonce_manager: Default::default(),
            provider: Default::default(),
            substrate_provider: Default::default(),
            use_fallback_gas_filler,
            node_start_timeout: polkadot_omnichain_node_configuration.start_timeout_ms,
            node_logging_level: polkadot_omnichain_node_configuration.logging_level.clone(),
            eth_rpc_logging_level: context.as_eth_rpc_configuration().logging_level.clone(),
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        trace!("Removing the various directories");
        let _ = remove_dir_all(self.base_directory_path.as_path());
        let _ = clear_directory(&self.base_directory_path);
        let _ = clear_directory(&self.logs_directory_path);

        trace!("Creating the various directories");
        create_dir_all(&self.base_directory_path)
            .context("Failed to create base directory for polkadot-omni-node node")?;
        create_dir_all(&self.logs_directory_path)
            .context("Failed to create logs directory for polkadot-omni-node node")?;

        let template_chainspec_path = self.base_directory_path.join(Self::CHAIN_SPEC_JSON_FILE);

        let chainspec_json = Self::node_genesis(
            &self.wallet,
            self.chain_spec_path
                .as_ref()
                .context("No runtime path provided")?,
        )
        .context("Failed to prepare the chainspec command")?;

        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)
                .context("Failed to create polkadot-omni-node template chainspec file")?,
            &chainspec_json,
        )
        .context("Failed to write polkadot-omni-node template chainspec JSON")?;

        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        // Error out if the runtime's path or the parachain id are not set which means that the
        // arguments we require were not provided.
        self.chain_spec_path
            .as_ref()
            .context("No WASM path provided for the runtime")?;
        self.parachain_id
            .as_ref()
            .context("No argument provided for the parachain-id")?;

        let polkadot_omnichain_node_rpc_port =
            Self::BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT + self.id as u16;
        let eth_rpc_port = Self::BASE_ETH_RPC_PORT + self.id as u16;

        let chainspec_path = self.base_directory_path.join(Self::CHAIN_SPEC_JSON_FILE);

        self.rpc_url = format!("http://127.0.0.1:{eth_rpc_port}");

        let polkadot_omnichain_node_process = Process::new(
            "node",
            self.logs_directory_path.as_path(),
            self.polkadot_omnichain_node_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--log")
                    .arg(self.node_logging_level.as_str())
                    .arg("--dev-block-time")
                    .arg(self.block_time.as_millis().to_string())
                    .arg("--rpc-port")
                    .arg(polkadot_omnichain_node_rpc_port.to_string())
                    .arg("--base-path")
                    .arg(self.base_directory_path.as_path())
                    .arg("--no-prometheus")
                    .arg("--no-hardware-benchmarks")
                    .arg("--authoring")
                    .arg("slot-based")
                    .arg("--chain")
                    .arg(chainspec_path)
                    .arg("--name")
                    .arg(format!("polkadot-omni-node-{}", self.id))
                    .arg("--rpc-methods")
                    .arg("unsafe")
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
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: self.node_start_timeout,
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::POLKADOT_OMNICHAIN_NODE_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );

        match polkadot_omnichain_node_process {
            Ok(process) => self.polkadot_omnichain_node_process = Some(process),
            Err(err) => {
                tracing::error!(
                    ?err,
                    "Failed to start polkadot-omni-node, shutting down gracefully"
                );
                self.shutdown().context(
                    "Failed to gracefully shutdown after polkadot-omni-node start error",
                )?;
                return Err(err);
            }
        }

        let eth_rpc_process = Process::new(
            "eth-rpc",
            self.logs_directory_path.as_path(),
            self.eth_rpc_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--rpc-port")
                    .arg(eth_rpc_port.to_string())
                    .arg("--node-rpc-url")
                    .arg(format!("ws://127.0.0.1:{polkadot_omnichain_node_rpc_port}"))
                    .arg("--rpc-max-connections")
                    .arg(NUMBER_OF_CACHED_BLOCKS.to_string())
                    .arg("--rpc-max-batch-request-len")
                    .arg(u32::MAX.to_string())
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
                tracing::error!(?err, "Failed to start eth-rpc, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth-rpc start error")?;
                return Err(err);
            }
        }

        Ok(())
    }

    pub fn eth_rpc_version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.eth_rpc_binary_path)
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
        wallet: &EthereumWallet,
        chain_spec_path: &Path,
    ) -> anyhow::Result<serde_json::Value> {
        let unmodified_chainspec_file =
            File::open(chain_spec_path).context("Failed to open the unmodified chainspec file")?;
        let mut chainspec_json =
            serde_json::from_reader::<_, serde_json::Value>(&unmodified_chainspec_file)
                .context("Failed to read the unmodified chainspec JSON")?;

        inject_wallet_balances(&mut chainspec_json, wallet)?;

        Ok(chainspec_json)
    }
}

impl NodeApi for PolkadotOmnichainNode {
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
        let substrate_rpc_port = Self::BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT + self.id as u16;
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

impl Node for PolkadotOmnichainNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        drop(self.polkadot_omnichain_node_process.take());
        drop(self.eth_rpc_process.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory_path.join("data"));

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.polkadot_omnichain_node_binary_path)
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

impl Drop for PolkadotOmnichainNode {
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}
