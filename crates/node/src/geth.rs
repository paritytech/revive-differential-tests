//! The go-ethereum node implementation.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions, create_dir_all, remove_dir_all},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use alloy::{
    eips::BlockNumberOrTag,
    network::{Ethereum, EthereumWallet},
    primitives::Address,
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{FillProvider, TxFiller},
    },
    rpc::types::{
        TransactionReceipt, TransactionRequest,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use revive_dt_config::Arguments;
use revive_dt_node_interaction::{BlockingExecutor, EthereumNode};
use tracing::Level;

use crate::Node;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The go-ethereum node instance implementation.
///
/// Implements helpers to initialize, spawn and wait the node.
///
/// Assumes dev mode and IPC only (`P2P`, `http`` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
pub struct Instance {
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
    nonces: Mutex<HashMap<Address, u64>>,
    /// This vector stores [`File`] objects that we use for logging which we want to flush when the
    /// node object is dropped. We do not store them in a structured fashion at the moment (in
    /// separate fields) as the logic that we need to apply to them is all the same regardless of
    /// what it belongs to, we just want to flush them on [`Drop`] of the node.
    logs_file_to_flush: Vec<File>,
}

impl Instance {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    const GETH_STDOUT_LOG_FILE_NAME: &str = "node_stdout.log";
    const GETH_STDERR_LOG_FILE_NAME: &str = "node_stderr.log";

    /// Create the node directory and call `geth init` to configure the genesis.
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, genesis: String) -> anyhow::Result<&mut Self> {
        create_dir_all(&self.base_directory)?;
        create_dir_all(&self.logs_directory)?;

        let genesis_path = self.base_directory.join(Self::GENESIS_JSON_FILE);
        File::create(&genesis_path)?.write_all(genesis.as_bytes())?;

        let mut child = Command::new(&self.geth)
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
        Box::pin(async move {
            ProviderBuilder::new()
                .wallet(wallet)
                .connect(&connection_string)
                .await
                .map_err(Into::into)
        })
    }
}

impl EthereumNode for Instance {
    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        let provider = self.provider();
        BlockingExecutor::execute(async move {
            let outer_span = tracing::debug_span!("Submitting transaction", ?transaction,);
            let _outer_guard = outer_span.enter();

            let provider = provider.await?;

            let pending_transaction = provider.send_transaction(transaction).await?;
            let transaction_hash = pending_transaction.tx_hash();

            let span = tracing::info_span!("Awaiting transaction receipt", ?transaction_hash);
            let _guard = span.enter();

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
            // At the moment we do not allow for the 60 seconds to be modified and we take it as
            // being an implementation detail that's invisible to anything outside of this module.
            //
            // We allow a total of 60 retries for getting the receipt with one second between each
            // retry and the next which means that we allow for a total of 60 seconds of waiting
            // before we consider that we're unable to get the transaction receipt.
            let mut retries = 0;
            loop {
                match provider.get_transaction_receipt(*transaction_hash).await {
                    Ok(Some(receipt)) => {
                        tracing::info!("Obtained the transaction receipt");
                        break Ok(receipt);
                    }
                    Ok(None) => {
                        if retries == 60 {
                            tracing::error!(
                                "Polled for transaction receipt for 60 seconds but failed to get it"
                            );
                            break Err(anyhow::anyhow!("Failed to get the transaction receipt"));
                        } else {
                            tracing::trace!(
                                retries,
                                "Sleeping for 1 second and trying to get the receipt again"
                            );
                            retries += 1;
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                    Err(error) => {
                        let error_string = error.to_string();
                        if error_string.contains("transaction indexing is in progress") {
                            if retries == 60 {
                                tracing::error!(
                                    "Polled for transaction receipt for 60 seconds but failed to get it"
                                );
                                break Err(error.into());
                            } else {
                                tracing::trace!(
                                    retries,
                                    "Sleeping for 1 second and trying to get the receipt again"
                                );
                                retries += 1;
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                continue;
                            }
                        } else {
                            break Err(error.into());
                        }
                    }
                }
            }
        })?
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn trace_transaction(
        &self,
        transaction: TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });
        let provider = self.provider();

        BlockingExecutor::execute(async move {
            Ok(provider
                .await?
                .debug_trace_transaction(transaction.transaction_hash, trace_options)
                .await?)
        })?
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn state_diff(
        &self,
        transaction: alloy::rpc::types::TransactionReceipt,
    ) -> anyhow::Result<DiffMode> {
        match self
            .trace_transaction(transaction)?
            .try_into_pre_state_frame()?
        {
            PreStateFrame::Diff(diff) => Ok(diff),
            _ => anyhow::bail!("expected a diff mode trace"),
        }
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn fetch_add_nonce(&self, address: Address) -> anyhow::Result<u64> {
        let provider = self.provider();
        let onchain_nonce = BlockingExecutor::execute::<anyhow::Result<_>>(async move {
            provider
                .await?
                .get_transaction_count(address)
                .await
                .map_err(Into::into)
        })??;

        let mut nonces = self.nonces.lock().unwrap();
        let current = nonces.entry(address).or_insert(onchain_nonce);
        let value = *current;
        *current += 1;
        Ok(value)
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn chain_id(&self) -> anyhow::Result<alloy::primitives::ChainId> {
        let provider = self.provider();
        BlockingExecutor::execute(async move {
            provider.await?.get_chain_id().await.map_err(Into::into)
        })?
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn gas_limit(&self) -> anyhow::Result<u128> {
        let provider = self.provider();
        BlockingExecutor::execute(async move {
            provider
                .await?
                .get_block_by_number(BlockNumberOrTag::Latest)
                .await?
                .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
                .map(|block| block.header.gas_limit as _)
        })?
    }

    #[tracing::instrument(skip_all, fields(geth_node_id = self.id))]
    fn coinbase(&self) -> anyhow::Result<Address> {
        let provider = self.provider();
        BlockingExecutor::execute(async move {
            provider
                .await?
                .get_block_by_number(BlockNumberOrTag::Latest)
                .await?
                .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
                .map(|block| block.header.beneficiary)
        })?
    }
}

impl Node for Instance {
    fn new(config: &Arguments) -> Self {
        let geth_directory = config.directory().join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

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
            wallet: config.wallet(),
            nonces: Mutex::new(HashMap::new()),
            // We know that we only need to be storing 2 files so we can specify that when creating
            // the vector. It's the stdout and stderr of the geth node.
            logs_file_to_flush: Vec::with_capacity(2),
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
}

impl Drop for Instance {
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

    fn new_node() -> (Instance, TempDir) {
        let (args, temp_dir) = test_config();
        let mut node = Instance::new(&args);
        node.init(GENESIS_JSON.to_owned())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (node, temp_dir)
    }

    #[test]
    fn init_works() {
        Instance::new(&test_config().0)
            .init(GENESIS_JSON.to_string())
            .unwrap();
    }

    #[test]
    fn spawn_works() {
        Instance::new(&test_config().0)
            .spawn(GENESIS_JSON.to_string())
            .unwrap();
    }

    #[test]
    fn version_works() {
        let version = Instance::new(&test_config().0).version().unwrap();
        assert!(
            version.starts_with("geth version"),
            "expected version string, got: '{version}'"
        );
    }

    #[test]
    fn can_get_chain_id_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let chain_id = node.chain_id();

        // Assert
        let chain_id = chain_id.expect("Failed to get the chain id");
        assert_eq!(chain_id, 420_420_420);
    }

    #[test]
    fn can_get_gas_limit_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let gas_limit = node.gas_limit();

        // Assert
        let gas_limit = gas_limit.expect("Failed to get the gas limit");
        assert_eq!(gas_limit, u32::MAX as u128)
    }

    #[test]
    fn can_get_coinbase_from_node() {
        // Arrange
        let (node, _temp_dir) = new_node();

        // Act
        let coinbase = node.coinbase();

        // Assert
        let coinbase = coinbase.expect("Failed to get the gas limit");
        assert_eq!(coinbase, Address::new([0xFF; 20]))
    }
}
