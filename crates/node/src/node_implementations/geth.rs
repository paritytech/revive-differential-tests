//! The go-ethereum node implementation.

use crate::internal_prelude::*;

#[derive(Debug)]
pub struct GethNode {
    id: u32,
    process: GethProcess,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    gas_filler: FallbackGasFiller,
    _directories: NodeDirectories,
}

impl GethNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasGethConfiguration
        + HasGenesisConfiguration
        + Clone,
        use_fallback_gas_filler: bool,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let genesis_config = context.as_genesis_configuration();
        let wallet_config = context.as_wallet_configuration();
        let geth_config = context.as_geth_configuration();

        let id = NodeId::for_node("geth");
        let directories =
            NodeDirectories::new(workdir_config.working_directory.as_path(), "geth", id.0)
                .context("Failed to initialize node directories")?;
        let ipc_path = directories.base_directory().join("geth.ipc");
        let genesis_path = directories.base_directory().join("genesis.json");

        let wallet = wallet_config.wallet();
        let mut genesis = genesis_config
            .genesis()
            .context("Failed to get genesis for geth")?
            .clone();
        for signer_address in NetworkWallet::<Ethereum>::signer_addresses(&wallet) {
            genesis
                .alloc
                .entry(signer_address)
                .or_insert(GenesisAccount::default().with_balance(U256::from(INITIAL_BALANCE)));
        }
        File::create(genesis_path.as_path())
            .context("Failed to create the genesis path file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &genesis).context("Failed to write genesis to file")
            })?;

        let process = GethProcess::new(
            geth_config.path.as_path(),
            genesis_path,
            ipc_path.as_path(),
            directories.data_directory(),
            directories.logs_directory(),
            geth_config.logging_level.as_str(),
            geth_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn geth"))?;

        Ok(Self {
            id: id.0,
            process,
            wallet,
            nonce_manager: CachedNonceManager::default(),
            provider: Default::default(),
            gas_filler: FallbackGasFiller::new().with_fallback_mechanism(use_fallback_gas_filler),
            _directories: directories,
        })
    }

    // TODO(no-genesis-export): Remove this function
    pub fn node_genesis(genesis: Genesis, _: &EthereumWallet) -> Genesis {
        genesis
    }
}

impl NodeApi for GethNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.process.url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn provider(&self) -> FrameworkFuture<Result<DynProvider>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_owned();
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
}

impl Node for GethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, _: Genesis) -> Result<()> {
        Ok(())
    }
}
