use std::{
    fs::{File, OpenOptions, create_dir_all, remove_dir_all},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use alloy::{
    consensus::{BlockHeader, TxEnvelope},
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{
        Ethereum, EthereumWallet, Network, NetworkWallet, TransactionBuilder,
        TransactionBuilderError, UnbuiltTransactionError,
    },
    primitives::{
        Address, B64, B256, BlockHash, BlockNumber, BlockTimestamp, Bloom, Bytes, FixedBytes,
        TxHash, U256,
    },
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        TransactionReceipt,
        eth::{Block, Header, Transaction},
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
    signers::local::PrivateKeySigner,
};
use anyhow::Context;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;
use tracing::Level;

use revive_dt_config::Arguments;
use revive_dt_node_interaction::EthereumNode;

use crate::{Node, common::FallbackGasFiller, constants::INITIAL_BALANCE};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct KitchensinkNode {
    id: u32,
    substrate_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    rpc_url: String,
    wallet: EthereumWallet,
    base_directory: PathBuf,
    logs_directory: PathBuf,
    process_substrate: Option<Child>,
    process_proxy: Option<Child>,
    nonce_manager: CachedNonceManager,
    /// This vector stores [`File`] objects that we use for logging which we want to flush when the
    /// node object is dropped. We do not store them in a structured fashion at the moment (in
    /// separate fields) as the logic that we need to apply to them is all the same regardless of
    /// what it belongs to, we just want to flush them on [`Drop`] of the node.
    logs_file_to_flush: Vec<File>,
}

impl KitchensinkNode {
    const BASE_DIRECTORY: &str = "kitchensink";
    const LOGS_DIRECTORY: &str = "logs";
    const DATA_DIRECTORY: &str = "chains";

    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    const SUBSTRATE_LOG_ENV: &str = "error,evm=debug,sc_rpc_server=info,runtime::revive=debug";
    const PROXY_LOG_ENV: &str = "info,eth-rpc=debug";

    const KITCHENSINK_STDOUT_LOG_FILE_NAME: &str = "node_stdout.log";
    const KITCHENSINK_STDERR_LOG_FILE_NAME: &str = "node_stderr.log";

    const PROXY_STDOUT_LOG_FILE_NAME: &str = "proxy_stdout.log";
    const PROXY_STDERR_LOG_FILE_NAME: &str = "proxy_stderr.log";

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn init(&mut self, genesis: &str) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)?;
        create_dir_all(&self.logs_directory)?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        // Note: we do not pipe the logs of this process to a separate file since this is just a
        // once-off export of the default chain spec and not part of the long-running node process.
        let output = Command::new(&self.substrate_binary)
            .arg("export-chain-spec")
            .arg("--chain")
            .arg("dev")
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "substrate-node export-chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)?;
        let mut chainspec_json: JsonValue = serde_json::from_str(&content)?;

        let existing_chainspec_balances =
            chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        let mut merged_balances: Vec<(String, u128)> = existing_chainspec_balances
            .into_iter()
            .filter_map(|val| {
                if let Some(arr) = val.as_array() {
                    if arr.len() == 2 {
                        let account = arr[0].as_str()?.to_string();
                        let balance = arr[1].as_f64()? as u128;
                        return Some((account, balance));
                    }
                }
                None
            })
            .collect();
        let mut eth_balances = {
            let mut genesis = serde_json::from_str::<Genesis>(genesis)?;
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
            self.extract_balance_from_genesis_file(&genesis)?
        };
        merged_balances.append(&mut eth_balances);

        chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"] =
            json!(merged_balances);

        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)?,
            &chainspec_json,
        )?;
        Ok(self)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        let chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

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

        // Start Substrate node
        let kitchensink_stdout_logs_file = open_options
            .clone()
            .open(self.kitchensink_stdout_log_file_path())?;
        let kitchensink_stderr_logs_file = open_options
            .clone()
            .open(self.kitchensink_stderr_log_file_path())?;
        self.process_substrate = Command::new(&self.substrate_binary)
            .arg("--dev")
            .arg("--chain")
            .arg(chainspec_path)
            .arg("--base-path")
            .arg(&self.base_directory)
            .arg("--rpc-port")
            .arg(substrate_rpc_port.to_string())
            .arg("--name")
            .arg(format!("revive-kitchensink-{}", self.id))
            .arg("--force-authoring")
            .arg("--rpc-methods")
            .arg("Unsafe")
            .arg("--rpc-cors")
            .arg("all")
            .env("RUST_LOG", Self::SUBSTRATE_LOG_ENV)
            .stdout(kitchensink_stdout_logs_file.try_clone()?)
            .stderr(kitchensink_stderr_logs_file.try_clone()?)
            .spawn()?
            .into();

        // Give the node a moment to boot
        if let Err(error) = Self::wait_ready(
            self.kitchensink_stderr_log_file_path().as_path(),
            Self::SUBSTRATE_READY_MARKER,
            Duration::from_secs(60),
        ) {
            tracing::error!(
                ?error,
                "Failed to start substrate, shutting down gracefully"
            );
            self.shutdown()?;
            return Err(error);
        };

        let eth_proxy_stdout_logs_file = open_options
            .clone()
            .open(self.proxy_stdout_log_file_path())?;
        let eth_proxy_stderr_logs_file = open_options.open(self.proxy_stderr_log_file_path())?;
        self.process_proxy = Command::new(&self.eth_proxy_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(proxy_rpc_port.to_string())
            .arg("--node-rpc-url")
            .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
            .env("RUST_LOG", Self::PROXY_LOG_ENV)
            .stdout(eth_proxy_stdout_logs_file.try_clone()?)
            .stderr(eth_proxy_stderr_logs_file.try_clone()?)
            .spawn()?
            .into();

        if let Err(error) = Self::wait_ready(
            self.proxy_stderr_log_file_path().as_path(),
            Self::ETH_PROXY_READY_MARKER,
            Duration::from_secs(60),
        ) {
            tracing::error!(?error, "Failed to start proxy, shutting down gracefully");
            self.shutdown()?;
            return Err(error);
        };

        self.logs_file_to_flush.extend([
            kitchensink_stdout_logs_file,
            kitchensink_stderr_logs_file,
            eth_proxy_stdout_logs_file,
            eth_proxy_stderr_logs_file,
        ]);

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn extract_balance_from_genesis_file(
        &self,
        genesis: &Genesis,
    ) -> anyhow::Result<Vec<(String, u128)>> {
        genesis
            .alloc
            .iter()
            .try_fold(Vec::new(), |mut vec, (address, acc)| {
                let substrate_address = Self::eth_to_substrate_address(address);
                let balance = acc.balance.try_into()?;
                vec.push((substrate_address, balance));
                Ok(vec)
            })
    }

    fn eth_to_substrate_address(address: &Address) -> String {
        let eth_bytes = address.0.0;

        let mut padded = [0xEEu8; 32];
        padded[..20].copy_from_slice(&eth_bytes);

        let account_id = AccountId32::from(padded);
        account_id.to_ss58check()
    }

    fn wait_ready(logs_file_path: &Path, marker: &str, timeout: Duration) -> anyhow::Result<()> {
        let start_time = std::time::Instant::now();
        let logs_file = OpenOptions::new()
            .read(true)
            .write(false)
            .append(false)
            .truncate(false)
            .open(logs_file_path)?;

        let mut lines = std::io::BufReader::new(logs_file).lines();
        loop {
            if let Some(Ok(line)) = lines.next() {
                if line.contains(marker) {
                    return Ok(());
                }
            }

            if start_time.elapsed() > timeout {
                anyhow::bail!("Timeout waiting for process readiness: {marker}");
            }
        }
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    pub fn eth_rpc_version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.eth_proxy_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id), level = Level::TRACE)]
    fn kitchensink_stdout_log_file_path(&self) -> PathBuf {
        self.logs_directory
            .join(Self::KITCHENSINK_STDOUT_LOG_FILE_NAME)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id), level = Level::TRACE)]
    fn kitchensink_stderr_log_file_path(&self) -> PathBuf {
        self.logs_directory
            .join(Self::KITCHENSINK_STDERR_LOG_FILE_NAME)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id), level = Level::TRACE)]
    fn proxy_stdout_log_file_path(&self) -> PathBuf {
        self.logs_directory.join(Self::PROXY_STDOUT_LOG_FILE_NAME)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id), level = Level::TRACE)]
    fn proxy_stderr_log_file_path(&self) -> PathBuf {
        self.logs_directory.join(Self::PROXY_STDERR_LOG_FILE_NAME)
    }

    fn provider(
        &self,
    ) -> impl Future<
        Output = anyhow::Result<
            FillProvider<
                impl TxFiller<KitchenSinkNetwork>,
                impl Provider<KitchenSinkNetwork>,
                KitchenSinkNetwork,
            >,
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
                .network::<KitchenSinkNetwork>()
                .filler(FallbackGasFiller::new(
                    30_000_000,
                    200_000_000_000,
                    3_000_000_000,
                ))
                .filler(ChainIdFiller::default())
                .filler(NonceFiller::new(nonce_manager))
                .wallet(wallet)
                .connect(&connection_string)
                .await
                .map_err(Into::into)
        })
    }
}

impl EthereumNode for KitchensinkNode {
    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn execute_transaction(
        &self,
        transaction: alloy::rpc::types::TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt> {
        tracing::debug!(?transaction, "Submitting transaction");
        let receipt = self
            .provider()
            .await?
            .send_transaction(transaction)
            .await?
            .get_receipt()
            .await?;
        tracing::info!(?receipt, "Submitted tx to kitchensink");
        Ok(receipt)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn trace_transaction(
        &self,
        transaction: &TransactionReceipt,
        trace_options: GethDebugTracingOptions,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let tx_hash = transaction.transaction_hash;
        Ok(self
            .provider()
            .await?
            .debug_trace_transaction(tx_hash, trace_options)
            .await?)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
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

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn balance_of(&self, address: Address) -> anyhow::Result<U256> {
        self.provider()
            .await?
            .get_balance(address)
            .await
            .map_err(Into::into)
    }
}

impl ResolverApi for KitchensinkNode {
    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn chain_id(&self) -> anyhow::Result<alloy::primitives::ChainId> {
        self.provider()
            .await?
            .get_chain_id()
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn transaction_gas_price(&self, tx_hash: &TxHash) -> anyhow::Result<u128> {
        self.provider()
            .await?
            .get_transaction_receipt(*tx_hash)
            .await?
            .context("Failed to get the transaction receipt")
            .map(|receipt| receipt.effective_gas_price)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn block_gas_limit(&self, number: BlockNumberOrTag) -> anyhow::Result<u128> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.gas_limit as _)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn block_coinbase(&self, number: BlockNumberOrTag) -> anyhow::Result<Address> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.beneficiary)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn block_difficulty(&self, number: BlockNumberOrTag) -> anyhow::Result<U256> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
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

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn block_hash(&self, number: BlockNumberOrTag) -> anyhow::Result<BlockHash> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.hash)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn block_timestamp(&self, number: BlockNumberOrTag) -> anyhow::Result<BlockTimestamp> {
        self.provider()
            .await?
            .get_block_by_number(number)
            .await?
            .ok_or(anyhow::Error::msg("Blockchain has no blocks"))
            .map(|block| block.header.timestamp)
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    async fn last_block_number(&self) -> anyhow::Result<BlockNumber> {
        self.provider()
            .await?
            .get_block_number()
            .await
            .map_err(Into::into)
    }
}

impl Node for KitchensinkNode {
    fn new(config: &Arguments) -> Self {
        let kitchensink_directory = config.directory().join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = kitchensink_directory.join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);

        let mut wallet = config.wallet();
        for signer in (1..=config.private_keys_to_add)
            .map(|id| U256::from(id))
            .map(|id| id.to_be_bytes::<32>())
            .map(|id| PrivateKeySigner::from_bytes(&FixedBytes(id)).unwrap())
        {
            wallet.register_signer(signer);
        }

        Self {
            id,
            substrate_binary: config.kitchensink.clone(),
            eth_proxy_binary: config.eth_proxy.clone(),
            rpc_url: String::new(),
            wallet,
            base_directory,
            logs_directory,
            process_substrate: None,
            process_proxy: None,
            nonce_manager: Default::default(),
            // We know that we only need to be storing 4 files so we can specify that when creating
            // the vector. It's the stdout and stderr of the substrate-node and the eth-rpc.
            logs_file_to_flush: Vec::with_capacity(4),
        }
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn connection_string(&self) -> String {
        self.rpc_url.clone()
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn shutdown(&mut self) -> anyhow::Result<()> {
        // Terminate the processes in a graceful manner to allow for the output to be flushed.
        if let Some(mut child) = self.process_proxy.take() {
            child
                .kill()
                .map_err(|error| anyhow::anyhow!("Failed to kill the proxy process: {error:?}"))?;
        }
        if let Some(mut child) = self.process_substrate.take() {
            child.kill().map_err(|error| {
                anyhow::anyhow!("Failed to kill the substrate process: {error:?}")
            })?;
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

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.init(&genesis)?.spawn_process()
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.substrate_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }

    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn matches_target(&self, targets: Option<&[String]>) -> bool {
        match targets {
            None => true,
            Some(targets) => targets.iter().any(|str| str.as_str() == "pvm"),
        }
    }
}

impl Drop for KitchensinkNode {
    #[tracing::instrument(skip_all, fields(kitchensink_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct KitchenSinkNetwork;

impl Network for KitchenSinkNetwork {
    type TxType = <Ethereum as Network>::TxType;

    type TxEnvelope = <Ethereum as Network>::TxEnvelope;

    type UnsignedTx = <Ethereum as Network>::UnsignedTx;

    type ReceiptEnvelope = <Ethereum as Network>::ReceiptEnvelope;

    type Header = KitchenSinkHeader;

    type TransactionRequest = <Ethereum as Network>::TransactionRequest;

    type TransactionResponse = <Ethereum as Network>::TransactionResponse;

    type ReceiptResponse = <Ethereum as Network>::ReceiptResponse;

    type HeaderResponse = Header<KitchenSinkHeader>;

    type BlockResponse = Block<Transaction<TxEnvelope>, Header<KitchenSinkHeader>>;
}

impl TransactionBuilder<KitchenSinkNetwork> for <Ethereum as Network>::TransactionRequest {
    fn chain_id(&self) -> Option<alloy::primitives::ChainId> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::chain_id(self)
    }

    fn set_chain_id(&mut self, chain_id: alloy::primitives::ChainId) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_chain_id(
            self, chain_id,
        )
    }

    fn nonce(&self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::nonce(self)
    }

    fn set_nonce(&mut self, nonce: u64) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_nonce(
            self, nonce,
        )
    }

    fn take_nonce(&mut self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::take_nonce(
            self,
        )
    }

    fn input(&self) -> Option<&alloy::primitives::Bytes> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::input(self)
    }

    fn set_input<T: Into<alloy::primitives::Bytes>>(&mut self, input: T) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_input(
            self, input,
        )
    }

    fn from(&self) -> Option<Address> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::from(self)
    }

    fn set_from(&mut self, from: Address) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_from(
            self, from,
        )
    }

    fn kind(&self) -> Option<alloy::primitives::TxKind> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::kind(self)
    }

    fn clear_kind(&mut self) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::clear_kind(
            self,
        )
    }

    fn set_kind(&mut self, kind: alloy::primitives::TxKind) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_kind(
            self, kind,
        )
    }

    fn value(&self) -> Option<alloy::primitives::U256> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::value(self)
    }

    fn set_value(&mut self, value: alloy::primitives::U256) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_value(
            self, value,
        )
    }

    fn gas_price(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::gas_price(self)
    }

    fn set_gas_price(&mut self, gas_price: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_gas_price(
            self, gas_price,
        )
    }

    fn max_fee_per_gas(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::max_fee_per_gas(
            self,
        )
    }

    fn set_max_fee_per_gas(&mut self, max_fee_per_gas: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_max_fee_per_gas(
            self, max_fee_per_gas
        )
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::max_priority_fee_per_gas(
            self,
        )
    }

    fn set_max_priority_fee_per_gas(&mut self, max_priority_fee_per_gas: u128) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_max_priority_fee_per_gas(
            self, max_priority_fee_per_gas
        )
    }

    fn gas_limit(&self) -> Option<u64> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::gas_limit(self)
    }

    fn set_gas_limit(&mut self, gas_limit: u64) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_gas_limit(
            self, gas_limit,
        )
    }

    fn access_list(&self) -> Option<&alloy::rpc::types::AccessList> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::access_list(
            self,
        )
    }

    fn set_access_list(&mut self, access_list: alloy::rpc::types::AccessList) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::set_access_list(
            self,
            access_list,
        )
    }

    fn complete_type(
        &self,
        ty: <KitchenSinkNetwork as Network>::TxType,
    ) -> Result<(), Vec<&'static str>> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::complete_type(
            self, ty,
        )
    }

    fn can_submit(&self) -> bool {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::can_submit(
            self,
        )
    }

    fn can_build(&self) -> bool {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::can_build(self)
    }

    fn output_tx_type(&self) -> <KitchenSinkNetwork as Network>::TxType {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::output_tx_type(
            self,
        )
    }

    fn output_tx_type_checked(&self) -> Option<<KitchenSinkNetwork as Network>::TxType> {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::output_tx_type_checked(
            self,
        )
    }

    fn prep_for_submission(&mut self) {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::prep_for_submission(
            self,
        )
    }

    fn build_unsigned(
        self,
    ) -> alloy::network::BuildResult<<KitchenSinkNetwork as Network>::UnsignedTx, KitchenSinkNetwork>
    {
        let result = <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::build_unsigned(
            self,
        );
        match result {
            Ok(unsigned_tx) => Ok(unsigned_tx),
            Err(UnbuiltTransactionError { request, error }) => {
                Err(UnbuiltTransactionError::<KitchenSinkNetwork> {
                    request,
                    error: match error {
                        TransactionBuilderError::InvalidTransactionRequest(tx_type, items) => {
                            TransactionBuilderError::InvalidTransactionRequest(tx_type, items)
                        }
                        TransactionBuilderError::UnsupportedSignatureType => {
                            TransactionBuilderError::UnsupportedSignatureType
                        }
                        TransactionBuilderError::Signer(error) => {
                            TransactionBuilderError::Signer(error)
                        }
                        TransactionBuilderError::Custom(error) => {
                            TransactionBuilderError::Custom(error)
                        }
                    },
                })
            }
        }
    }

    async fn build<W: alloy::network::NetworkWallet<KitchenSinkNetwork>>(
        self,
        wallet: &W,
    ) -> Result<
        <KitchenSinkNetwork as Network>::TxEnvelope,
        TransactionBuilderError<KitchenSinkNetwork>,
    > {
        Ok(wallet.sign_request(self).await?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KitchenSinkHeader {
    /// The Keccak 256-bit hash of the parent
    /// block’s header, in its entirety; formally Hp.
    pub parent_hash: B256,
    /// The Keccak 256-bit hash of the ommers list portion of this block; formally Ho.
    #[serde(rename = "sha3Uncles", alias = "ommersHash")]
    pub ommers_hash: B256,
    /// The 160-bit address to which all fees collected from the successful mining of this block
    /// be transferred; formally Hc.
    #[serde(rename = "miner", alias = "beneficiary")]
    pub beneficiary: Address,
    /// The Keccak 256-bit hash of the root node of the state trie, after all transactions are
    /// executed and finalisations applied; formally Hr.
    pub state_root: B256,
    /// The Keccak 256-bit hash of the root node of the trie structure populated with each
    /// transaction in the transactions list portion of the block; formally Ht.
    pub transactions_root: B256,
    /// The Keccak 256-bit hash of the root node of the trie structure populated with the receipts
    /// of each transaction in the transactions list portion of the block; formally He.
    pub receipts_root: B256,
    /// The Bloom filter composed from indexable information (logger address and log topics)
    /// contained in each log entry from the receipt of each transaction in the transactions list;
    /// formally Hb.
    pub logs_bloom: Bloom,
    /// A scalar value corresponding to the difficulty level of this block. This can be calculated
    /// from the previous block’s difficulty level and the timestamp; formally Hd.
    pub difficulty: U256,
    /// A scalar value equal to the number of ancestor blocks. The genesis block has a number of
    /// zero; formally Hi.
    #[serde(with = "alloy::serde::quantity")]
    pub number: BlockNumber,
    /// A scalar value equal to the current limit of gas expenditure per block; formally Hl.
    // This is the main difference over the Ethereum network implementation. We use u128 here and
    // not u64.
    #[serde(with = "alloy::serde::quantity")]
    pub gas_limit: u128,
    /// A scalar value equal to the total gas used in transactions in this block; formally Hg.
    #[serde(with = "alloy::serde::quantity")]
    pub gas_used: u64,
    /// A scalar value equal to the reasonable output of Unix’s time() at this block’s inception;
    /// formally Hs.
    #[serde(with = "alloy::serde::quantity")]
    pub timestamp: u64,
    /// An arbitrary byte array containing data relevant to this block. This must be 32 bytes or
    /// fewer; formally Hx.
    pub extra_data: Bytes,
    /// A 256-bit hash which, combined with the
    /// nonce, proves that a sufficient amount of computation has been carried out on this block;
    /// formally Hm.
    pub mix_hash: B256,
    /// A 64-bit value which, combined with the mixhash, proves that a sufficient amount of
    /// computation has been carried out on this block; formally Hn.
    pub nonce: B64,
    /// A scalar representing EIP1559 base fee which can move up or down each block according
    /// to a formula which is a function of gas used in parent block and gas target
    /// (block gas limit divided by elasticity multiplier) of parent block.
    /// The algorithm results in the base fee per gas increasing when blocks are
    /// above the gas target, and decreasing when blocks are below the gas target. The base fee per
    /// gas is burned.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_fee_per_gas: Option<u64>,
    /// The Keccak 256-bit hash of the withdrawals list portion of this block.
    /// <https://eips.ethereum.org/EIPS/eip-4895>
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawals_root: Option<B256>,
    /// The total amount of blob gas consumed by the transactions within the block, added in
    /// EIP-4844.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub blob_gas_used: Option<u64>,
    /// A running total of blob gas consumed in excess of the target, prior to the block. Blocks
    /// with above-target blob gas consumption increase this value, blocks with below-target blob
    /// gas consumption decrease it (bounded at 0). This was added in EIP-4844.
    #[serde(
        default,
        with = "alloy::serde::quantity::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub excess_blob_gas: Option<u64>,
    /// The hash of the parent beacon block's root is included in execution blocks, as proposed by
    /// EIP-4788.
    ///
    /// This enables trust-minimized access to consensus state, supporting staking pools, bridges,
    /// and more.
    ///
    /// The beacon roots contract handles root storage, enhancing Ethereum's functionalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_beacon_block_root: Option<B256>,
    /// The Keccak 256-bit hash of the an RLP encoded list with each
    /// [EIP-7685] request in the block body.
    ///
    /// [EIP-7685]: https://eips.ethereum.org/EIPS/eip-7685
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_hash: Option<B256>,
}

impl BlockHeader for KitchenSinkHeader {
    fn parent_hash(&self) -> B256 {
        self.parent_hash
    }

    fn ommers_hash(&self) -> B256 {
        self.ommers_hash
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }

    fn state_root(&self) -> B256 {
        self.state_root
    }

    fn transactions_root(&self) -> B256 {
        self.transactions_root
    }

    fn receipts_root(&self) -> B256 {
        self.receipts_root
    }

    fn withdrawals_root(&self) -> Option<B256> {
        self.withdrawals_root
    }

    fn logs_bloom(&self) -> Bloom {
        self.logs_bloom
    }

    fn difficulty(&self) -> U256 {
        self.difficulty
    }

    fn number(&self) -> BlockNumber {
        self.number
    }

    // There's sadly nothing that we can do about this. We're required to implement this trait on
    // any type that represents a header and the gas limit type used here is a u64.
    fn gas_limit(&self) -> u64 {
        self.gas_limit.try_into().unwrap_or(u64::MAX)
    }

    fn gas_used(&self) -> u64 {
        self.gas_used
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn mix_hash(&self) -> Option<B256> {
        Some(self.mix_hash)
    }

    fn nonce(&self) -> Option<B64> {
        Some(self.nonce)
    }

    fn base_fee_per_gas(&self) -> Option<u64> {
        self.base_fee_per_gas
    }

    fn blob_gas_used(&self) -> Option<u64> {
        self.blob_gas_used
    }

    fn excess_blob_gas(&self) -> Option<u64> {
        self.excess_blob_gas
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.parent_beacon_block_root
    }

    fn requests_hash(&self) -> Option<B256> {
        self.requests_hash
    }

    fn extra_data(&self) -> &Bytes {
        &self.extra_data
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;
    use revive_dt_config::Arguments;
    use std::path::PathBuf;
    use std::sync::{LazyLock, Mutex};

    use std::fs;

    use super::*;
    use crate::{GENESIS_JSON, Node};

    fn test_config() -> Arguments {
        Arguments {
            kitchensink: PathBuf::from("substrate-node"),
            eth_proxy: PathBuf::from("eth-rpc"),
            ..Default::default()
        }
    }

    fn new_node() -> (KitchensinkNode, Arguments) {
        // Note: When we run the tests in the CI we found that if they're all
        // run in parallel then the CI is unable to start all of the nodes in
        // time and their start up times-out. Therefore, we want all of the
        // nodes to be started in series and not in parallel. To do this, we use
        // a dummy mutex here such that there can only be a single node being
        // started up at any point of time. This will make our tests run slower
        // but it will allow the node startup to not timeout.
        //
        // Note: an alternative to starting all of the nodes in series and not
        // in parallel would be for us to reuse the same node between tests
        // which is not the best thing to do in my opinion as it removes all
        // of the isolation between tests and makes them depend on what other
        // tests do. For example, if one test checks what the block number is
        // and another test submits a transaction then the tx test would have
        // side effects that affect the block number test.
        static NODE_START_MUTEX: Mutex<()> = Mutex::new(());
        let _guard = NODE_START_MUTEX.lock().unwrap();

        let args = test_config();
        let mut node = KitchensinkNode::new(&args);
        node.init(GENESIS_JSON)
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (node, args)
    }

    /// A shared node that multiple tests can use. It starts up once.
    fn shared_node() -> &'static KitchensinkNode {
        static NODE: LazyLock<(KitchensinkNode, Arguments)> = LazyLock::new(|| {
            let (node, args) = new_node();
            (node, args)
        });
        &NODE.0
    }

    #[tokio::test]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (node, args) = new_node();

        let provider = node.provider().await.expect("Failed to create provider");

        let account_address = args.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let receipt = provider.send_transaction(transaction).await;

        // Assert
        let _ = receipt
            .expect("Failed to send the transfer transaction")
            .get_receipt()
            .await
            .expect("Failed to get the receipt for the transfer");
    }

    #[test]
    fn test_init_generates_chainspec_with_balances() {
        let genesis_content = r#"
        {
            "alloc": {
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1": {
                    "balance": "1000000000000000000"
                },
                "Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2": {
                    "balance": "2000000000000000000"
                }
            }
        }
        "#;

        let mut dummy_node = KitchensinkNode::new(&test_config());

        // Call `init()`
        dummy_node.init(genesis_content).expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = dummy_node
            .base_directory
            .join(KitchensinkNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents = fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Substrate addresses derived from the Ethereum addresses are in the file
        let first_eth_addr = KitchensinkNode::eth_to_substrate_address(
            &"90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap(),
        );
        let second_eth_addr = KitchensinkNode::eth_to_substrate_address(
            &"Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2".parse().unwrap(),
        );

        assert!(
            contents.contains(&first_eth_addr),
            "Chainspec should contain Substrate address for first Ethereum account"
        );
        assert!(
            contents.contains(&second_eth_addr),
            "Chainspec should contain Substrate address for second Ethereum account"
        );
    }

    #[test]
    fn test_parse_genesis_alloc() {
        // Create test genesis file
        let genesis_json = r#"
        {
          "alloc": {
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1": { "balance": "1000000000000000000" },
            "0x0000000000000000000000000000000000000000": { "balance": "0xDE0B6B3A7640000" },
            "0xffffffffffffffffffffffffffffffffffffffff": { "balance": "123456789" }
          }
        }
        "#;

        let node = KitchensinkNode::new(&test_config());

        let result = node
            .extract_balance_from_genesis_file(&serde_json::from_str(genesis_json).unwrap())
            .unwrap();

        let result_map: std::collections::HashMap<_, _> = result.into_iter().collect();

        assert_eq!(
            result_map.get("5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5C4hrfjw9DjXZTzV3MwzrrAr9P1MLDHajjSidz9bR544LEq1"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5HrN7fHLXWcFiXPwwtq2EkSGns9eMmoUQnbVKweNz3VVr6N4"),
            Some(&123_456_789u128)
        );
    }

    #[test]
    fn print_eth_to_substrate_mappings() {
        let eth_addresses = vec![
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
            "0xffffffffffffffffffffffffffffffffffffffff",
            "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
        ];

        for eth_addr in eth_addresses {
            let ss58 = KitchensinkNode::eth_to_substrate_address(&eth_addr.parse().unwrap());

            println!("Ethereum: {eth_addr} -> Substrate SS58: {ss58}");
        }
    }

    #[test]
    fn test_eth_to_substrate_address() {
        let cases = vec![
            (
                "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "0x0000000000000000000000000000000000000000",
                "5C4hrfjw9DjXZTzV3MwzrrAr9P1MLDHajjSidz9bR544LEq1",
            ),
            (
                "0xffffffffffffffffffffffffffffffffffffffff",
                "5HrN7fHLXWcFiXPwwtq2EkSGns9eMmoUQnbVKweNz3VVr6N4",
            ),
        ];

        for (eth_addr, expected_ss58) in cases {
            let result = KitchensinkNode::eth_to_substrate_address(&eth_addr.parse().unwrap());
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    fn spawn_works() {
        let config = test_config();

        let mut node = KitchensinkNode::new(&config);

        node.spawn(GENESIS_JSON.to_string()).unwrap();
    }

    #[test]
    fn version_works() {
        let config = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected substrate-node version string, got: {version}"
        );
    }

    #[test]
    fn eth_rpc_version_works() {
        let config = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }

    #[tokio::test]
    async fn can_get_chain_id_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let chain_id = node.chain_id().await;

        // Assert
        let chain_id = chain_id.expect("Failed to get the chain id");
        assert_eq!(chain_id, 420_420_420);
    }

    #[tokio::test]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let gas_limit = node.block_gas_limit(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = gas_limit.expect("Failed to get the gas limit");
    }

    #[tokio::test]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let coinbase = node.block_coinbase(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = coinbase.expect("Failed to get the coinbase");
    }

    #[tokio::test]
    async fn can_get_block_difficulty_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_difficulty = node.block_difficulty(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = block_difficulty.expect("Failed to get the block difficulty");
    }

    #[tokio::test]
    async fn can_get_block_hash_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_hash = node.block_hash(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = block_hash.expect("Failed to get the block hash");
    }

    #[tokio::test]
    async fn can_get_block_timestamp_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_timestamp = node.block_timestamp(BlockNumberOrTag::Latest).await;

        // Assert
        let _ = block_timestamp.expect("Failed to get the block timestamp");
    }

    #[tokio::test]
    async fn can_get_block_number_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_number = node.last_block_number().await;

        // Assert
        let _ = block_number.expect("Failed to get the block number");
    }
}
