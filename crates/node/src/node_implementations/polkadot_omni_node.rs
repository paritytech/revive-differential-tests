#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A node implementation for the polkadot-omni-node.
#[derive(Debug)]

pub struct PolkadotOmnichainNode {
    /// The id of the node.
    id: u32,

    /// The path of the polkadot-omni-chain node binary.
    polkadot_omnichain_node_binary_path: PathBuf,
    /// The path of the eth-rpc binary.
    eth_rpc_binary: PathBuf,
    /// The path of the runtime's WASM that this node will be spawned with.
    chain_spec_path: Option<PathBuf>,
    directories: NodeDirectories,

    /// Defines the amount of time to wait before considering that the node start has timed out.
    node_start_timeout: Duration,

    /// The id of the parachain that this node will be spawning.
    parachain_id: Option<usize>,
    /// The block time.
    block_time: Duration,

    /// The node's process.
    polkadot_omnichain_node_process: Option<PolkadotOmniNodeProcess>,
    /// The eth-rpc's process.
    eth_rpc_process: Option<EthRpcProcess>,

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
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";

    /// Returns the WebSocket URL for the substrate RPC endpoint of this node.
    fn substrate_ws_url(&self) -> String {
        self.polkadot_omnichain_node_process
            .as_ref()
            .expect("must be initialized")
            .url()
            .to_string()
    }

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
        let directories = NodeDirectories::new(working_directory_path, "polkadot-omni-node", id)
            .expect("TODO(constructors): Remove this when we have failing constructors");

        Self {
            id,
            polkadot_omnichain_node_binary_path: polkadot_omnichain_node_configuration
                .path
                .to_path_buf(),
            eth_rpc_binary: eth_rpc_path.to_path_buf(),
            chain_spec_path: polkadot_omnichain_node_configuration
                .chain_spec_path
                .clone(),
            directories,
            parachain_id: polkadot_omnichain_node_configuration.parachain_id,
            block_time: polkadot_omnichain_node_configuration.block_time_ms,
            polkadot_omnichain_node_process: Default::default(),
            eth_rpc_process: Default::default(),
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
        let template_chainspec_path = self
            .directories
            .base_directory()
            .join(Self::CHAIN_SPEC_JSON_FILE);

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
        let chainspec_path = self
            .directories
            .base_directory()
            .join(Self::CHAIN_SPEC_JSON_FILE);

        self.polkadot_omnichain_node_process = PolkadotOmniNodeProcess::new(
            self.polkadot_omnichain_node_binary_path.as_path(),
            self.block_time,
            chainspec_path,
            self.directories.data_directory(),
            self.directories.logs_directory(),
            self.node_logging_level.as_str(),
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn polkadot-omni-node"))?
        .into();

        self.eth_rpc_process = EthRpcProcess::new(
            self.eth_rpc_binary.as_path(),
            self.directories.logs_directory(),
            self.polkadot_omnichain_node_process
                .as_ref()
                .expect("qed; we initialized it")
                .url(),
            self.eth_rpc_logging_level.as_str(),
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?
        .into();

        Ok(())
    }

    async fn provider(&self) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.connection_string(),
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
        self.eth_rpc_process
            .as_ref()
            .expect("must be initialized")
            .url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn provider(&self) -> revive_dt_common::futures::FrameworkFuture<anyhow::Result<DynProvider>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_string();
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
        let connection_string = self.substrate_ws_url();

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

    fn substrate_rpc_client(
        &self,
    ) -> Option<FrameworkFuture<Result<subxt::backend::rpc::RpcClient>>> {
        let url = self.substrate_ws_url();
        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(url)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for PolkadotOmnichainNode {
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }
}
