use crate::internal_prelude::*;

#[derive(Debug)]
pub struct PolkadotOmnichainNode {
    id: u32,
    eth_rpc_process: EthRpcProcess,
    polkadot_omnichain_node_process: PolkadotOmniNodeProcess,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    gas_filler: FallbackGasFiller,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,
    _directories: NodeDirectories,
}

impl PolkadotOmnichainNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasPolkadotOmnichainNodeConfiguration,
        use_fallback_gas_filler: bool,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let wallet_config = context.as_wallet_configuration();
        let node_config = context.as_polkadot_omnichain_node_configuration();
        let rpc_config = context.as_eth_rpc_configuration();

        let source_chainspec_path = node_config
            .chain_spec_path
            .as_ref()
            .context("No chain spec path provided for the polkadot-omni-node")?;
        node_config
            .parachain_id
            .context("No argument provided for the parachain-id")?;

        let id = NodeId::for_node("polkadot-omni-node");
        let directories = NodeDirectories::new(
            workdir_config.working_directory.as_path(),
            "polkadot-omni-node",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let chainspec_path = directories.base_directory().join("chainspec.json");

        let wallet = wallet_config.wallet();
        Self::init_chainspec(&wallet, source_chainspec_path, chainspec_path.as_path())
            .context("Failed to initialize the chainspec file")?;

        let polkadot_omnichain_node_process = PolkadotOmniNodeProcess::new(
            node_config.path.as_path(),
            node_config.block_time_ms,
            chainspec_path,
            directories.data_directory(),
            directories.logs_directory(),
            node_config.logging_level.as_str(),
            node_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn polkadot-omni-node"))?;

        let eth_rpc_process = EthRpcProcess::new(
            rpc_config.path.as_path(),
            directories.logs_directory(),
            polkadot_omnichain_node_process.url(),
            rpc_config.logging_level.as_str(),
            rpc_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?;

        Ok(Self {
            id: id.0,
            eth_rpc_process,
            polkadot_omnichain_node_process,
            wallet,
            nonce_manager: Default::default(),
            gas_filler: FallbackGasFiller::new().with_fallback_mechanism(use_fallback_gas_filler),
            provider: Default::default(),
            substrate_provider: Default::default(),
            _directories: directories,
        })
    }

    fn init_chainspec(
        wallet: &EthereumWallet,
        chain_spec_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
    ) -> Result<()> {
        let chainspec =
            Self::chainspec(wallet, chain_spec_path).context("Failed to create the chainspec")?;
        File::create(output_path.as_ref())
            .context("Failed to create the chainspec file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &chainspec)
                    .context("Failed to serialize chainspec to writer")
            })?;
        Ok(())
    }

    pub fn chainspec(wallet: &EthereumWallet, chain_spec_path: impl AsRef<Path>) -> Result<Value> {
        let mut chainspec = File::open(chain_spec_path.as_ref())
            .context("Failed to open the chainspec file")
            .map(BufReader::new)
            .and_then(|reader| {
                serde_json::from_reader::<_, Value>(reader)
                    .context("Failed to deserialize chainspec file")
            })?;
        inject_wallet_balances(&mut chainspec, wallet)?;
        Ok(chainspec)
    }
}

impl NodeApi for PolkadotOmnichainNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.eth_rpc_process.url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn provider(&self) -> StaticFuture<Result<DynProvider>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_string();
        let gas_filler = self.gas_filler;
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

    fn substrate_provider(&self) -> Option<StaticFuture<Result<OnlineClient<PolkadotConfig>>>> {
        let provider = self.substrate_provider.clone();
        let connection_string = self.polkadot_omnichain_node_process.url().to_string();

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
    ) -> Option<StaticFuture<Result<subxt::backend::rpc::RpcClient>>> {
        let url = self.polkadot_omnichain_node_process.url().to_string();
        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(url)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for PolkadotOmnichainNode {
    fn spawn(&mut self, _: Genesis) -> Result<()> {
        Ok(())
    }
}
