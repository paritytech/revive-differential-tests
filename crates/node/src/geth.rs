//! The go-ethereum node implementation.

use std::{
    collections::HashMap,
    fs::{File, create_dir_all, remove_dir_all},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::{Provider, ProviderBuilder, ext::DebugApi},
    rpc::types::{
        TransactionReceipt, TransactionRequest,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use revive_dt_config::Arguments;
use revive_dt_node_interaction::{
    EthereumNode, nonce::fetch_onchain_nonce, trace::trace_transaction,
    transaction::execute_transaction,
};

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
    geth: PathBuf,
    id: u32,
    handle: Option<Child>,
    network_id: u64,
    start_timeout: u64,
    wallet: EthereumWallet,
    nonces: Mutex<HashMap<Address, u64>>,
}

impl Instance {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    /// Create the node directory and call `geth init` to configure the genesis.
    fn init(&mut self, genesis: String) -> anyhow::Result<&mut Self> {
        create_dir_all(&self.base_directory)?;

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
    /// [Instance::init] must be called priorly.
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
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
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?
            .into();
        Ok(self)
    }

    /// Wait for the g-ethereum node child process getting ready.
    ///
    /// [Instance::spawn_process] must be called priorly.
    fn wait_ready(&mut self) -> anyhow::Result<&mut Self> {
        // Thanks clippy but geth is a server; we don't `wait` but eventually kill it.
        #[allow(clippy::zombie_processes)]
        let mut child = self.handle.take().expect("should be spawned");
        let start_time = Instant::now();
        let maximum_wait_time = Duration::from_millis(self.start_timeout);
        let mut stderr = BufReader::new(child.stderr.take().expect("should be piped")).lines();
        let error = loop {
            let Some(Ok(line)) = stderr.next() else {
                break "child process stderr reading error".to_string();
            };
            if line.contains(Self::ERROR_MARKER) {
                break line;
            }
            if line.contains(Self::READY_MARKER) {
                // Keep stderr alive
                // https://github.com/alloy-rs/alloy/issues/2091#issuecomment-2676134147
                thread::spawn(move || for _ in stderr.by_ref() {});

                self.handle = child.into();
                return Ok(self);
            }
            if Instant::now().duration_since(start_time) > maximum_wait_time {
                break "spawn timeout".to_string();
            }
        };

        let _ = child.kill();
        anyhow::bail!("geth node #{} spawn error: {error}", self.id)
    }
}

impl EthereumNode for Instance {
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        let connection_string = self.connection_string();
        let wallet = self.wallet.clone();

        execute_transaction(Box::pin(async move {
            let outer_span = tracing::debug_span!("Submitting transaction", ?transaction,);
            let _outer_guard = outer_span.enter();

            let provider = ProviderBuilder::new()
                .wallet(wallet)
                .connect(&connection_string)
                .await?;

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
        }))
    }

    fn trace_transaction(
        &self,
        transaction: TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let connection_string = self.connection_string();
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });
        let wallet = self.wallet.clone();

        trace_transaction(Box::pin(async move {
            Ok(ProviderBuilder::new()
                .wallet(wallet)
                .connect(&connection_string)
                .await?
                .debug_trace_transaction(transaction.transaction_hash, trace_options)
                .await?)
        }))
    }

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

    fn fetch_add_nonce(&self, address: Address) -> anyhow::Result<u64> {
        let connection_string = self.connection_string.clone();
        let wallet = self.wallet.clone();

        let onchain_nonce = fetch_onchain_nonce(connection_string, wallet, address)?;

        let mut nonces = self.nonces.lock().unwrap();
        let current = nonces.entry(address).or_insert(onchain_nonce);
        let value = *current;
        *current += 1;
        Ok(value)
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
            base_directory,
            geth: config.geth.clone(),
            id,
            handle: None,
            network_id: config.network_id,
            start_timeout: config.geth_start_timeout,
            wallet: config.wallet(),
            nonces: Mutex::new(HashMap::new()),
        }
    }

    fn connection_string(&self) -> String {
        self.connection_string.clone()
    }

    fn shutdown(self) -> anyhow::Result<()> {
        Ok(())
    }

    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?.wait_ready()?;
        Ok(())
    }

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
    fn drop(&mut self) {
        tracing::info!(id = self.id, "Dropping node");
        if let Some(child) = self.handle.as_mut() {
            let _ = child.kill();
        }
        if self.base_directory.exists() {
            let _ = remove_dir_all(&self.base_directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use revive_dt_config::Arguments;
    use temp_dir::TempDir;

    use crate::{GENESIS_JSON, Node};

    use super::Instance;

    fn test_config() -> (Arguments, TempDir) {
        let mut config = Arguments::default();
        let temp_dir = TempDir::new().unwrap();
        config.working_directory = temp_dir.path().to_path_buf().into();

        (config, temp_dir)
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
}
