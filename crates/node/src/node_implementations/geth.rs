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
    base_directory: PathBuf,
    data_directory: PathBuf,
    logs_directory: PathBuf,
    geth: PathBuf,
    id: u32,
    handle: Option<Process>,
    start_timeout: Duration,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    use_fallback_gas_filler: bool,
    node_logging_level: String,
}

impl GethNode {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

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

        let geth_directory = working_directory_configuration
            .working_directory
            .as_path()
            .join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        let wallet = wallet_configuration.wallet();

        Self {
            connection_string: base_directory.join(Self::IPC_FILE).display().to_string(),
            data_directory: base_directory.join(Self::DATA_DIRECTORY),
            logs_directory: base_directory.join(Self::LOGS_DIRECTORY),
            base_directory,
            geth: geth_configuration.path.clone(),
            id,
            handle: None,
            start_timeout: geth_configuration.start_timeout_ms,
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
            use_fallback_gas_filler,
            node_logging_level: geth_configuration.logging_level.clone(),
        }
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for geth node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for geth node")?;

        let genesis = Self::node_genesis(genesis, self.wallet.as_ref());
        let genesis_path = self.base_directory.join(Self::GENESIS_JSON_FILE);
        serde_json::to_writer(
            File::create(&genesis_path).context("Failed to create geth genesis file")?,
            &genesis,
        )
        .context("Failed to serialize geth genesis JSON to file")?;

        let mut child = Command::new(&self.geth)
            .arg("--state.scheme")
            .arg("hash")
            .arg("init")
            .arg("--datadir")
            .arg(&self.data_directory)
            .arg(genesis_path)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("Failed to spawn geth --init process")?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)
            .context("Failed to read geth --init stderr")?;

        if !child
            .wait()
            .context("Failed waiting for geth --init process to finish")?
            .success()
        {
            anyhow::bail!("failed to initialize geth node #{:?}: {stderr}", &self.id);
        }

        Ok(self)
    }

    /// Spawn the go-ethereum node child process.
    ///
    /// [Instance::init] must be called prior.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let process = Process::new(
            None,
            self.logs_directory.as_path(),
            self.geth.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--datadir")
                    .arg(&self.data_directory)
                    .arg("--ipcpath")
                    .arg(&self.connection_string)
                    .arg("--nodiscover")
                    .arg("--maxpeers")
                    .arg("0")
                    .arg("--txlookuplimit")
                    .arg("0")
                    .arg("--cache.blocklogs")
                    .arg("512")
                    .arg("--state.scheme")
                    .arg("hash")
                    .arg("--syncmode")
                    .arg("full")
                    .arg("--gcmode")
                    .arg("archive")
                    .arg("--verbosity")
                    .arg(self.node_logging_level.as_str())
                    .arg("--rpc.batch-request-limit")
                    .arg("0")
                    .stderr(stderr_file)
                    .stdout(stdout_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: self.start_timeout,
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => {
                        if line.contains(Self::ERROR_MARKER) {
                            anyhow::bail!("Failed to start geth {line}");
                        } else if line.contains(Self::READY_MARKER) {
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    }
                    None => Ok(false),
                }),
            },
        );

        match process {
            Ok(process) => self.handle = Some(process),
            Err(err) => {
                error!(?err, "Failed to start geth, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after geth start error")?;
                return Err(err);
            }
        }

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
    fn shutdown(&mut self) -> anyhow::Result<()> {
        drop(self.handle.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY));

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.geth)
            .arg("--version")
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

impl Drop for GethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
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

    fn shared_node() -> &'static GethNode {
        &shared_state().1
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

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn version_works() {
        // Arrange
        let node = shared_node();

        // Act
        let version = node.version();

        // Assert
        let version = version.expect("Failed to get the version");
        assert!(
            version.starts_with("geth version"),
            "expected version string, got: '{version}'"
        );
    }
}
