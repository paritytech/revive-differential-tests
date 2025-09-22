//! The go-ethereum node implementation.

use std::{
    fs::{File, create_dir_all, remove_dir_all},
    io::Read,
    ops::ControlFlow,
    path::PathBuf,
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use alloy::{
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256},
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionRequest,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use anyhow::Context as _;
use revive_common::EVMVersion;
use tracing::{Instrument, instrument};

use revive_dt_common::{
    fs::clear_directory,
    futures::{PollingWaitBehavior, poll},
};
use revive_dt_config::*;
use revive_dt_format::traits::ResolverApi;
use revive_dt_node_interaction::EthereumNode;

use crate::{
    Node,
    common::FallbackGasFiller,
    constants::INITIAL_BALANCE,
    process::{Process, ProcessReadinessWaitBehavior},
};

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
    chain_id_filler: ChainIdFiller,
}

impl GethNode {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    const TRANSACTION_INDEXING_ERROR: &str = "transaction indexing is in progress";
    const TRANSACTION_TRACING_ERROR: &str = "historical state not available in path scheme yet";

    const RECEIPT_POLLING_DURATION: Duration = Duration::from_secs(5 * 60);
    const TRACE_POLLING_DURATION: Duration = Duration::from_secs(60);

    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<GethConfiguration>
        + Clone,
    ) -> Self {
        let working_directory_configuration =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let wallet_configuration = AsRef::<WalletConfiguration>::as_ref(&context);
        let geth_configuration = AsRef::<GethConfiguration>::as_ref(&context);

        let geth_directory = working_directory_configuration
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
            chain_id_filler: Default::default(),
            nonce_manager: Default::default(),
        }
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, mut genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for geth node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for geth node")?;

        for signer_address in
            <EthereumWallet as NetworkWallet<Ethereum>>::signer_addresses(&self.wallet)
        {
            // Note, the use of the entry API here means that we only modify the entries for any
            // account that is not in the `alloc` field of the genesis state.
            genesis
                .alloc
                .entry(signer_address)
                .or_insert(GenesisAccount::default().with_balance(U256::from(INITIAL_BALANCE)));
        }
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
                tracing::error!(?err, "Failed to start geth, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after geth start error")?;
                return Err(err);
            }
        }

        Ok(self)
    }

    async fn provider(
        &self,
    ) -> anyhow::Result<FillProvider<impl TxFiller<Ethereum>, impl Provider<Ethereum>, Ethereum>>
    {
        ProviderBuilder::new()
            .disable_recommended_fillers()
            .filler(FallbackGasFiller::new(
                25_000_000,
                1_000_000_000,
                1_000_000_000,
            ))
            .filler(self.chain_id_filler.clone())
            .filler(NonceFiller::new(self.nonce_manager.clone()))
            .wallet(self.wallet.clone())
            .connect(&self.connection_string)
            .await
            .map_err(Into::into)
    }
}

impl EthereumNode for GethNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.connection_string
    }

    #[instrument(
        level = "info",
        skip_all,
        fields(geth_node_id = self.id, connection_string = self.connection_string),
        err,
    )]
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::rpc::types::TransactionReceipt>> + '_>>
    {
        Box::pin(async move {
            let provider = self
                .provider()
                .await
                .context("Failed to create provider for transaction submission")?;

            let pending_transaction = provider
            .send_transaction(transaction)
            .await
            .inspect_err(
                |err| tracing::error!(%err, "Encountered an error when submitting the transaction"),
            )
            .context("Failed to submit transaction to geth node")?;
            let transaction_hash = *pending_transaction.tx_hash();

            // The following is a fix for the "transaction indexing is in progress" error that we used
            // to get. You can find more information on this in the following GH issue in geth
            // https://github.com/ethereum/go-ethereum/issues/28877. To summarize what's going on,
            // before we can get the receipt of the transaction it needs to have been indexed by the
            // node's indexer. Just because the transaction has been confirmed it doesn't mean that it
            // has been indexed. When we call alloy's `get_receipt` it checks if the transaction was
            // confirmed. If it has been, then it will call `eth_getTransactionReceipt` method which
            // _might_ return the above error if the tx has not yet been indexed yet. So, we need to
            // implement a retry mechanism for the receipt to keep retrying to get it until it
            // eventually works, but we only do that if the error we get back is the "transaction
            // indexing is in progress" error or if the receipt is None.
            //
            // Getting the transaction indexed and taking a receipt can take a long time especially when
            // a lot of transactions are being submitted to the node. Thus, while initially we only
            // allowed for 60 seconds of waiting with a 1 second delay in polling, we need to allow for
            // a larger wait time. Therefore, in here we allow for 5 minutes of waiting with exponential
            // backoff each time we attempt to get the receipt and find that it's not available.
            let provider = Arc::new(provider);
            poll(
                Self::RECEIPT_POLLING_DURATION,
                PollingWaitBehavior::Constant(Duration::from_millis(200)),
                move || {
                    let provider = provider.clone();
                    async move {
                        match provider.get_transaction_receipt(transaction_hash).await {
                            Ok(Some(receipt)) => Ok(ControlFlow::Break(receipt)),
                            Ok(None) => Ok(ControlFlow::Continue(())),
                            Err(error) => {
                                let error_string = error.to_string();
                                match error_string.contains(Self::TRANSACTION_INDEXING_ERROR) {
                                    true => Ok(ControlFlow::Continue(())),
                                    false => Err(error.into()),
                                }
                            }
                        }
                    }
                },
            )
            .instrument(tracing::info_span!(
                "Awaiting transaction receipt",
                ?transaction_hash
            ))
            .await
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::rpc::types::trace::geth::GethTrace>> + '_>>
    {
        Box::pin(async move {
            let provider = Arc::new(
                self.provider()
                    .await
                    .context("Failed to create provider for tracing")?,
            );
            poll(
                Self::TRACE_POLLING_DURATION,
                PollingWaitBehavior::Constant(Duration::from_millis(200)),
                move || {
                    let provider = provider.clone();
                    let trace_options = trace_options.clone();
                    async move {
                        match provider
                            .debug_trace_transaction(tx_hash, trace_options)
                            .await
                        {
                            Ok(trace) => Ok(ControlFlow::Break(trace)),
                            Err(error) => {
                                let error_string = error.to_string();
                                match error_string.contains(Self::TRANSACTION_TRACING_ERROR) {
                                    true => Ok(ControlFlow::Continue(())),
                                    false => Err(error.into()),
                                }
                            }
                        }
                    }
                },
            )
            .await
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn state_diff(
        &self,
        tx_hash: TxHash,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<DiffMode>> + '_>> {
        Box::pin(async move {
            let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
                diff_mode: Some(true),
                disable_code: None,
                disable_storage: None,
            });
            match self
                .trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to trace transaction for prestate diff")?
                .try_into_pre_state_frame()
                .context("Failed to convert trace into pre-state frame")?
            {
                PreStateFrame::Diff(diff) => Ok(diff),
                _ => anyhow::bail!("expected a diff mode trace"),
            }
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn balance_of(
        &self,
        address: Address,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to get the Geth provider")?
                .get_balance(address)
                .await
                .map_err(Into::into)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<EIP1186AccountProofResponse>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to get the Geth provider")?
                .get_proof(address, keys)
                .latest()
                .await
                .map_err(Into::into)
        })
    }

    // #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn resolver(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Arc<dyn ResolverApi + '_>>> + '_>> {
        Box::pin(async move {
            let id = self.id;
            let provider = self.provider().await?;
            Ok(Arc::new(GethNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
        })
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }
}

pub struct GethNodeResolver<F: TxFiller<Ethereum>, P: Provider<Ethereum>> {
    id: u32,
    provider: FillProvider<F, P, Ethereum>,
}

impl<F: TxFiller<Ethereum>, P: Provider<Ethereum>> ResolverApi for GethNodeResolver<F, P> {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn chain_id(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::primitives::ChainId>> + '_>> {
        Box::pin(async move { self.provider.get_chain_id().await.map_err(Into::into) })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn transaction_gas_price(
        &self,
        tx_hash: TxHash,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u128>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_transaction_receipt(tx_hash)
                .await?
                .context("Failed to get the transaction receipt")
                .map(|receipt| receipt.effective_gas_price)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_gas_limit(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u128>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .map(|block| block.header.gas_limit as _)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_coinbase(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Address>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .map(|block| block.header.beneficiary)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_difficulty(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_base_fee(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u64>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .and_then(|block| {
                    block
                        .header
                        .base_fee_per_gas
                        .context("Failed to get the base fee per gas")
                })
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_hash(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockHash>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .map(|block| block.header.hash)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockTimestamp>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the geth block")?
                .context("Failed to get the Geth block, perhaps there are no blocks?")
                .map(|block| block.header.timestamp)
        })
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn last_block_number(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockNumber>> + '_>> {
        Box::pin(async move { self.provider.get_block_number().await.map_err(Into::into) })
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
    use super::*;

    fn test_config() -> TestExecutionContext {
        TestExecutionContext::default()
    }

    fn new_node() -> (TestExecutionContext, GethNode) {
        let context = test_config();
        let mut node = GethNode::new(&context);
        node.init(context.genesis_configuration.genesis().unwrap().clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    #[test]
    fn version_works() {
        let version = GethNode::new(&test_config()).version().unwrap();
        assert!(
            version.starts_with("geth version"),
            "expected version string, got: '{version}'"
        );
    }

    #[tokio::test]
    async fn can_get_chain_id_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let chain_id = node.resolver().await.unwrap().chain_id().await;

        // Assert
        let chain_id = chain_id.expect("Failed to get the chain id");
        assert_eq!(chain_id, 420_420_420);
    }

    #[tokio::test]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let gas_limit = node
            .resolver()
            .await
            .unwrap()
            .block_gas_limit(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let gas_limit = gas_limit.expect("Failed to get the gas limit");
        assert_eq!(gas_limit, u32::MAX as u128)
    }

    #[tokio::test]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let coinbase = node
            .resolver()
            .await
            .unwrap()
            .block_coinbase(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let coinbase = coinbase.expect("Failed to get the coinbase");
        assert_eq!(coinbase, Address::new([0xFF; 20]))
    }

    #[tokio::test]
    async fn can_get_block_difficulty_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let block_difficulty = node
            .resolver()
            .await
            .unwrap()
            .block_difficulty(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let block_difficulty = block_difficulty.expect("Failed to get the block difficulty");
        assert_eq!(block_difficulty, U256::ZERO)
    }

    #[tokio::test]
    async fn can_get_block_hash_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let block_hash = node
            .resolver()
            .await
            .unwrap()
            .block_hash(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let _ = block_hash.expect("Failed to get the block hash");
    }

    #[tokio::test]
    async fn can_get_block_timestamp_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let block_timestamp = node
            .resolver()
            .await
            .unwrap()
            .block_timestamp(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let _ = block_timestamp.expect("Failed to get the block timestamp");
    }

    #[tokio::test]
    async fn can_get_block_number_from_node() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let block_number = node.resolver().await.unwrap().last_block_number().await;

        // Assert
        let block_number = block_number.expect("Failed to get the block number");
        assert_eq!(block_number, 0)
    }
}
