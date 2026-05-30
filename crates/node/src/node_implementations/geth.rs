//! The go-ethereum node implementation.

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The go-ethereum node instance implementation.
///
/// Implements helpers to initialize, spawn and wait the node.
///
/// Assumes dev mode and IPC only (`P2P`, `http`` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
#[allow(clippy::type_complexity)]
pub struct GethNode {
    connection_string: String,
    directories: NodeDirectories,
    geth: PathBuf,
    id: u32,
    process: Option<GethProcess>,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    use_fallback_gas_filler: bool,
    node_logging_level: String,
}

impl GethNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasGethConfiguration
        + Clone,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let working_directory_configuration = context.as_working_directory_configuration();
        let wallet_configuration = context.as_wallet_configuration();
        let geth_configuration = context.as_geth_configuration();

        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let directories = NodeDirectories::new(
            working_directory_configuration.working_directory.as_path(),
            "geth",
            id,
        )
        .expect("TODO(constructors): Remove this when we have failing constructors");

        let wallet = wallet_configuration.wallet();

        Self {
            connection_string: directories
                .base_directory()
                .join("geth.ipc")
                .display()
                .to_string(),
            directories,
            geth: geth_configuration.path.clone(),
            id,
            process: Default::default(),
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
            use_fallback_gas_filler,
            node_logging_level: geth_configuration.logging_level.clone(),
        }
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        let genesis = Self::node_genesis(genesis, self.wallet.as_ref());
        let genesis_path = self.directories.base_directory().join("genesis.json");
        serde_json::to_writer(
            File::create(&genesis_path).context("Failed to create geth genesis file")?,
            &genesis,
        )
        .context("Failed to serialize geth genesis JSON to file")?;
        Ok(self)
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let genesis_path = self.directories.base_directory().join("genesis.json");

        self.process = GethProcess::new(
            self.geth.as_path(),
            genesis_path,
            self.connection_string.as_str(),
            self.directories.data_directory(),
            self.directories.logs_directory(),
            self.node_logging_level.as_str(),
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn geth"))?
        .into();

        Ok(self)
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

impl NodeApi for GethNode {
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
}

impl Node for GethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use alloy::rpc::types::TransactionRequest;

    use super::*;

    fn test_config() -> Test {
        Test::default()
    }

    fn new_node() -> (Test, GethNode) {
        let context = test_config();
        let mut node = GethNode::new(context.clone(), true);
        node.init(context.genesis.genesis().unwrap().clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    fn shared_state() -> &'static (Test, GethNode) {
        static STATE: LazyLock<(Test, GethNode)> = LazyLock::new(new_node);
        &STATE
    }

    #[tokio::test]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = shared_state();

        let account_address = context.wallet.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let receipt = node.execute_transaction(transaction).await;

        // Assert
        let _ = receipt.expect("Failed to get the receipt for the transfer");
    }
}
