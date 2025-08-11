//! The go-ethereum node implementation.

use std::{
    fs::{File, OpenOptions, create_dir_all, remove_dir_all},
    io::{BufRead, BufReader, Read, Write},
    ops::ControlFlow,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use alloy::{
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, FixedBytes, TxHash, U256},
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        TransactionReceipt, TransactionRequest,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
    signers::local::PrivateKeySigner,
};
use anyhow::Context;
use tracing::{Instrument, Level};

use revive_dt_common::{fs::clear_directory, futures::poll};
use revive_dt_config::Arguments;
use revive_dt_format::traits::ResolverApi;
use revive_dt_node_interaction::EthereumNode;

use crate::{Node, common::FallbackGasFiller, constants::INITIAL_BALANCE};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The go-ethereum node instance implementation.
///
/// Implements helpers to initialize, spawn and wait the node.
///
/// Assumes dev mode and IPC only (`P2P`, `http`` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
pub struct GethNode {
    connection_string: String,
    base_directory: PathBuf,
    data_directory: PathBuf,
    logs_directory: PathBuf,
    geth: PathBuf,
    id: u32,
    handle: Option<Child>,
    network_id: u64,
    start_timeout: u64,
    wallet: EthereumWallet,
    nonce_manager: CachedNonceManager,
    /// This vector stores [`File`] objects that we use for logging which we want to flush when the
    /// node object is dropped. We do not store them in a structured fashion at the moment (in
    /// separate fields) as the logic that we need to apply to them is all the same regardless of
    /// what it belongs to, we just want to flush them on [`Drop`] of the node.
    logs_file_to_flush: Vec<File>,
}

impl GethNode {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    const GETH_STDOUT_LOG_FILE_NAME: &str = "node_stdout.log";
    const GETH_STDERR_LOG_FILE_NAME: &str = "node_stderr.log";

    const TRANSACTION_INDEXING_ERROR: &str = "transaction indexing is in progress";
    const TRANSACTION_TRACING_ERROR: &str = "historical state not available in path scheme yet";

    const RECEIPT_POLLING_DURATION: Duration = Duration::from_secs(5 * 60);
    const TRACE_POLLING_DURATION: Duration = Duration::from_secs(60);

    /// Create the node directory and call `geth init` to configure the genesis.
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, genesis: String) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)?;
        create_dir_all(&self.logs_directory)?;

        let mut genesis = serde_json::from_str::<Genesis>(&genesis)?;
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
        serde_json::to_writer(File::create(&genesis_path)?, &genesis)?;

        let mut child = Command::new(&self.geth)
            .arg("--state.scheme")
            .arg("hash")
            .arg("init")
            .arg("--datadir")
            .arg(&self.data_directory)
            .arg(genesis_path)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)?;

        if !child.wait()?.success() {
            anyhow::bail!("failed to initialize geth node #{:?}: {stderr}", &self.id);
        }

        Ok(self)
    }

    /// Spawn the go-ethereum node child process.
    ///
    /// [Instance::init] must be called prior.
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        // This is the `OpenOptions` that we wish to use for all of the log files that we will be
        // opening in this method. We need to construct it in this way to:
        // 1. Be consistent
        // 2. Less verbose and more dry
        // 3. Because the builder pattern uses mutable references so we need to get around that.
        let open_options = {
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            options
        };

        let stdout_logs_file = open_options
            .clone()
            .open(self.geth_stdout_log_file_path())?;
        let stderr_logs_file = open_options.open(self.geth_stderr_log_file_path())?;
        self.handle = Command::new(&self.geth)
            .arg("--dev")
            .arg("--datadir")
            .arg(&self.data_directory)
            .arg("--ipcpath")
            .arg(&self.connection_string)
            .arg("--networkid")
            .arg(self.network_id.to_string())
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
            .stderr(stderr_logs_file.try_clone()?)
            .stdout(stdout_logs_file.try_clone()?)
            .spawn()?
            .into();

        if let Err(error) = self.wait_ready() {
            tracing::error!(?error, "Failed to start geth, shutting down gracefully");
            self.shutdown()?;
            return Err(error);
        }

        self.logs_file_to_flush
            .extend([stderr_logs_file, stdout_logs_file]);

        Ok(self)
    }

    /// Wait for the g-ethereum node child process getting ready.
    ///
    /// [Instance::spawn_process] must be called priorly.
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn wait_ready(&mut self) -> anyhow::Result<&mut Self> {
        let start_time = Instant::now();

        let logs_file = OpenOptions::new()
            .read(true)
            .write(false)
            .append(false)
            .truncate(false)
            .open(self.geth_stderr_log_file_path())?;

        let maximum_wait_time = Duration::from_millis(self.start_timeout);
        let mut stderr = BufReader::new(logs_file).lines();
        loop {
            if let Some(Ok(line)) = stderr.next() {
                if line.contains(Self::ERROR_MARKER) {
                    anyhow::bail!("Failed to start geth {line}");
                }
                if line.contains(Self::READY_MARKER) {
                    return Ok(self);
                }
            }
            if Instant::now().duration_since(start_time) > maximum_wait_time {
                anyhow::bail!("Timeout in starting geth");
            }
        }
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id), level = Level::TRACE)]
    fn geth_stdout_log_file_path(&self) -> PathBuf {
        self.logs_directory.join(Self::GETH_STDOUT_LOG_FILE_NAME)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id), level = Level::TRACE)]
    fn geth_stderr_log_file_path(&self) -> PathBuf {
        self.logs_directory.join(Self::GETH_STDERR_LOG_FILE_NAME)
    }

    fn provider(
        &self,
    ) -> impl Future<
        Output = anyhow::Result<
            FillProvider<impl TxFiller<Ethereum>, impl Provider<Ethereum>, Ethereum>,
        >,
    > + 'static {
        let connection_string = self.connection_string();
        let wallet = self.wallet.clone();

        // Note: We would like all providers to make use of the same nonce manager so that we have
        // monotonically increasing nonces that are cached. The cached nonce manager uses Arc's in
        // its implementation and therefore it means that when we clone it then it still references
        // the same state.
        let nonce_manager = self.nonce_manager.clone();

        Box::pin(async move {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .filler(FallbackGasFiller::new(500_000_000, 500_000_000, 1))
                .filler(ChainIdFiller::default())
                .filler(NonceFiller::new(nonce_manager))
                .wallet(wallet)
                .connect(&connection_string)
                .await
                .map_err(Into::into)
        })
    }
}

impl EthereumNode for GethNode {
    #[tracing::instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    async fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        let span = tracing::debug_span!("Submitting transaction", ?transaction);
        let _guard = span.enter();

        let provider = Arc::new(self.provider().await?);
        let transaction_hash = *provider.send_transaction(transaction).await?.tx_hash();

        // The following is a fix for the "transaction indexing is in progress" error that we
        // used to get. You can find more information on this in the following GH issue in geth
        // https://github.com/ethereum/go-ethereum/issues/28877. To summarize what's going on,
        // before we can get the receipt of the transaction it needs to have been indexed by the
        // node's indexer. Just because the transaction has been confirmed it doesn't mean that
        // it has been indexed. When we call alloy's `get_receipt` it checks if the transaction
        // was confirmed. If it has been, then it will call `eth_getTransactionReceipt` method
        // which _might_ return the above error if the tx has not yet been indexed yet. So, we
        // need to implement a retry mechanism for the receipt to keep retrying to get it until
        // it eventually works, but we only do that if the error we get back is the "transaction
        // indexing is in progress" error or if the receipt is None.
        //
        // Getting the transaction indexed and taking a receipt can take a long time especially
        // when a lot of transactions are being submitted to the node. Thus, while initially we
        // only allowed for 60 seconds of waiting with a 1 second delay in polling, we need to
        // allow for a larger wait time. Therefore, in here we allow for 5 minutes of waiting
        // with exponential backoff each time we attempt to get the receipt and find that it's
        // not available.
        poll(
            Self::RECEIPT_POLLING_DURATION,
            Default::default(),
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
    }

    #[tracing::instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    async fn trace_transaction(
        &self,
        transaction: &TransactionReceipt,
        trace_options: GethDebugTracingOptions,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let provider = Arc::new(self.provider().await?);
        poll(
            Self::TRACE_POLLING_DURATION,
            Default::default(),
            move || {
                let provider = provider.clone();
                let trace_options = trace_options.clone();
                async move {
                    match provider
                        .debug_trace_transaction(transaction.transaction_hash, trace_options)
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
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn state_diff(&self, transaction: &TransactionReceipt) -> anyhow::Result<DiffMode> {
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });
        match self
            .trace_transaction(transaction, trace_options)
            .await?
            .try_into_pre_state_frame()?
        {
            PreStateFrame::Diff(diff) => Ok(diff),
            _ => anyhow::bail!("expected a diff mode trace"),
        }
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn balance_of(&self, address: Address) -> anyhow::Result<U256> {
        self.provider()
            .await?
            .get_balance(address)
            .await
            .map_err(Into::into)
    }
}

impl ResolverApi for GethNode {
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn chain_id(&self) -> anyhow::Result<alloy::primitives::ChainId> {
        self.provider()
            .await?
            .get_chain_id()
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn transaction_gas_price(&self, tx_hash: &TxHash) -> anyhow::Result<u128> {
        self.provider()
            .await?
            .get_transaction_receipt(*tx_hash)
            .await?
            .context("Failed to get the transaction receipt")
            .map(|receipt| receipt.effective_gas_price)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_gas_limit(&self, number: BlockNumberOrTag) -> anyhow::Result<u128> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.gas_limit as _)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_coinbase(&self, number: BlockNumberOrTag) -> anyhow::Result<Address> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.beneficiary)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_difficulty(&self, number: BlockNumberOrTag) -> anyhow::Result<U256> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_base_fee(&self, number: BlockNumberOrTag) -> anyhow::Result<u64> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .and_then(|block| {
                block
                    .header
                    .base_fee_per_gas
                    .context("Failed to get the base fee per gas")
            })
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_hash(&self, number: BlockNumberOrTag) -> anyhow::Result<BlockHash> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.hash)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn block_timestamp(&self, number: BlockNumberOrTag) -> anyhow::Result<BlockTimestamp> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.timestamp)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    async fn last_block_number(&self) -> anyhow::Result<BlockNumber> {
        self.provider()
            .await?
            .get_block_number()
            .await
            .map_err(Into::into)
    }
}

impl Node for GethNode {
    fn new(config: &Arguments) -> Self {
        let geth_directory = config.directory().join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        let mut wallet = config.wallet();
        for signer in (1..=config.private_keys_to_add)
            .map(|id| U256::from(id))
            .map(|id| id.to_be_bytes::<32>())
            .map(|id| PrivateKeySigner::from_bytes(&FixedBytes(id)).unwrap())
        {
            wallet.register_signer(signer);
        }

        Self {
            connection_string: base_directory.join(Self::IPC_FILE).display().to_string(),
            data_directory: base_directory.join(Self::DATA_DIRECTORY),
            logs_directory: base_directory.join(Self::LOGS_DIRECTORY),
            base_directory,
            geth: config.geth.clone(),
            id,
            handle: None,
            network_id: config.network_id,
            start_timeout: config.geth_start_timeout,
            wallet,
            // We know that we only need to be storing 2 files so we can specify that when creating
            // the vector. It's the stdout and stderr of the geth node.
            logs_file_to_flush: Vec::with_capacity(2),
            nonce_manager: Default::default(),
        }
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn connection_string(&self) -> String {
        self.connection_string.clone()
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn shutdown(&mut self) -> anyhow::Result<()> {
        // Terminate the processes in a graceful manner to allow for the output to be flushed.
        if let Some(mut child) = self.handle.take() {
            child
                .kill()
                .map_err(|error| anyhow::anyhow!("Failed to kill the geth process: {error:?}"))?;
        }

        // Flushing the files that we're using for keeping the logs before shutdown.
        for file in self.logs_file_to_flush.iter_mut() {
            file.flush()?
        }

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY));

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.geth)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn matches_target(&self, targets: Option<&[String]>) -> bool {
        match targets {
            None => true,
            Some(targets) => targets.iter().any(|str| str.as_str() == "evm"),
        }
    }
}

impl Drop for GethNode {
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[cfg(test)]
mod tests {
    use revive_dt_config::Arguments;

    use temp_dir::TempDir;

    use crate::{GENESIS_JSON, Node};

    use super::*;

    fn test_config() -> (Arguments, TempDir) {
        let mut config = Arguments::default();
        let temp_dir = TempDir::new().unwrap();
        config.working_directory = temp_dir.path().to_path_buf().into();

        (config, temp_dir)
    }

    fn new_node() -> (GethNode, TempDir) {
        let (args, temp_dir) = test_config();
        let mut node = GethNode::new(&args);
        node.init(GENESIS_JSON.to_owned())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (node, temp_dir)
    }

    #[test]
    fn init_works() {
        GethNode::new(&test_config().0)
            .init(GENESIS_JSON.to_string())
            .unwrap();
    }

    #[test]
    fn spawn_works() {
        GethNode::new(&test_config().0)
            .spawn(GENESIS_JSON.to_string())
            .unwrap();
    }

    #[test]
    fn version_works() {
        let version = GethNode::new(&test_config().0).version().unwrap();
        assert!(
            version.starts_with("geth version"),
            "expected version string, got: '{version}'"
        );
    }

    #[tokio::test]
    async fn can_get_chain_id_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let chain_id = node.chain_id().await;

        // Assert
        let chain_id = chain_id.expect("Failed to get the chain id");
        assert_eq!(chain_id, 420_420_420);
    }

    #[tokio::test]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let gas_limit = node.block_gas_limit(BlockNumberOrTag::Latest).await;

        // Assert
        let gas_limit = gas_limit.expect("Failed to get the gas limit");
        assert_eq!(gas_limit, u32::MAX as u128)
    }

    #[tokio::test]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let coinbase = node.block_coinbase(BlockNumberOrTag::Latest).await;

        // Assert
        let coinbase = coinbase.expect("Failed to get the coinbase");
        assert_eq!(coinbase, Address::new([0xFF; 20]))
    }

    #[tokio::test]
    async fn can_get_block_difficulty_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let block_difficulty = node.block_difficulty(BlockNumberOrTag::Latest).await;

        // Assert
        let block_difficulty = block_difficulty.expect("Failed to get the block difficulty");
        assert_eq!(block_difficulty, U256::ZERO)
    }

    #[tokio::test]
    async fn can_get_block_hash_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let block_hash = node.block_hash(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = block_hash.expect("Failed to get the block hash");
    }

    #[tokio::test]
    async fn can_get_block_timestamp_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let block_timestamp = node.block_timestamp(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = block_timestamp.expect("Failed to get the block timestamp");
    }

    #[tokio::test]
    async fn can_get_block_number_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let block_number = node.last_block_number().await;

        // Assert
        let block_number = block_number.expect("Failed to get the block number");
        assert_eq!(block_number, 0)
    }
}
