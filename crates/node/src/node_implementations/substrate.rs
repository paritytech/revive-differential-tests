use std::{
    fs::{create_dir_all, remove_dir_all},
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
    consensus::{BlockHeader, TxEnvelope},
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{
        Ethereum, EthereumWallet, Network, NetworkWallet, TransactionBuilder,
        TransactionBuilderError, UnbuiltTransactionError,
    },
    primitives::{
        Address, B64, B256, BlockHash, BlockNumber, BlockTimestamp, Bloom, Bytes, StorageKey,
        TxHash, U256,
    },
    providers::{
        Provider,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, NonceFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest,
        eth::{Block, Header, Transaction},
        trace::geth::{
            DiffMode, GethDebugTracingOptions, GethTrace, PreStateConfig, PreStateFrame,
        },
    },
};
use anyhow::Context as _;
use futures::{Stream, StreamExt};
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;

use revive_dt_config::*;
use revive_dt_node_interaction::{EthereumNode, MinedBlockInformation};
use tokio::sync::OnceCell;
use tracing::instrument;

use crate::{
    Node,
    constants::{CHAIN_ID, INITIAL_BALANCE},
    helpers::{Process, ProcessReadinessWaitBehavior},
    provider_utils::{ConcreteProvider, FallbackGasFiller, construct_concurrency_limited_provider},
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A node implementation for Substrate based chains. Currently, this supports either substrate
/// or the revive-dev-node which is done by changing the path and some of the other arguments passed
/// to the command.
#[derive(Debug)]

pub struct SubstrateNode {
    id: u32,
    node_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    export_chainspec_command: String,
    rpc_url: String,
    base_directory: PathBuf,
    logs_directory: PathBuf,
    substrate_process: Option<Process>,
    eth_proxy_process: Option<Process>,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: OnceCell<ConcreteProvider<ReviveNetwork, Arc<EthereumWallet>>>,
}

impl SubstrateNode {
    const BASE_DIRECTORY: &str = "Substrate";
    const LOGS_DIRECTORY: &str = "logs";
    const DATA_DIRECTORY: &str = "chains";

    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    const SUBSTRATE_LOG_ENV: &str = "error,evm=debug,sc_rpc_server=info,runtime::revive=debug";
    const PROXY_LOG_ENV: &str = "info,eth-rpc=debug";

    pub const KITCHENSINK_EXPORT_CHAINSPEC_COMMAND: &str = "export-chain-spec";
    pub const REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND: &str = "build-spec";

    pub fn new(
        node_path: PathBuf,
        export_chainspec_command: &str,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>,
    ) -> Self {
        let working_directory_path =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context).as_path();
        let eth_rpc_path = AsRef::<EthRpcConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        let substrate_directory = working_directory_path.join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = substrate_directory.join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);

        Self {
            id,
            node_binary: node_path,
            eth_proxy_binary: eth_rpc_path.to_path_buf(),
            export_chainspec_command: export_chainspec_command.to_string(),
            rpc_url: String::new(),
            base_directory,
            logs_directory,
            substrate_process: None,
            eth_proxy_process: None,
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
        }
    }

    fn init(&mut self, mut genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = remove_dir_all(self.base_directory.as_path());
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for substrate node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for substrate node")?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        // Note: we do not pipe the logs of this process to a separate file since this is just a
        // once-off export of the default chain spec and not part of the long-running node process.
        let output = Command::new(&self.node_binary)
            .arg(self.export_chainspec_command.as_str())
            .arg("--chain")
            .arg("dev")
            .output()
            .context("Failed to export the chain-spec")?;

        if !output.status.success() {
            anyhow::bail!(
                "Substrate-node export-chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)
            .context("Failed to decode Substrate export-chain-spec output as UTF-8")?;
        let mut chainspec_json: JsonValue =
            serde_json::from_str(&content).context("Failed to parse Substrate chain spec JSON")?;

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
            self.extract_balance_from_genesis_file(&genesis)
                .context("Failed to extract balances from EVM genesis JSON")?
        };
        merged_balances.append(&mut eth_balances);

        chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"] =
            json!(merged_balances);

        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)
                .context("Failed to create substrate template chainspec file")?,
            &chainspec_json,
        )
        .context("Failed to write substrate template chainspec JSON")?;
        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        let chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        let substrate_process = Process::new(
            "node",
            self.logs_directory.as_path(),
            self.node_binary.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--chain")
                    .arg(chainspec_path)
                    .arg("--base-path")
                    .arg(&self.base_directory)
                    .arg("--rpc-port")
                    .arg(substrate_rpc_port.to_string())
                    .arg("--name")
                    .arg(format!("revive-substrate-{}", self.id))
                    .arg("--force-authoring")
                    .arg("--rpc-methods")
                    .arg("Unsafe")
                    .arg("--rpc-cors")
                    .arg("all")
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .env("RUST_LOG", Self::SUBSTRATE_LOG_ENV)
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(30),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::SUBSTRATE_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );
        match substrate_process {
            Ok(process) => self.substrate_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start substrate, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after substrate start error")?;
                return Err(err);
            }
        }

        let eth_proxy_process = Process::new(
            "proxy",
            self.logs_directory.as_path(),
            self.eth_proxy_binary.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--rpc-port")
                    .arg(proxy_rpc_port.to_string())
                    .arg("--node-rpc-url")
                    .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .env("RUST_LOG", Self::PROXY_LOG_ENV)
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(30),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::ETH_PROXY_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );
        match eth_proxy_process {
            Ok(process) => self.eth_proxy_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start eth proxy, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth proxy start error")?;
                return Err(err);
            }
        }

        Ok(())
    }

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

    async fn provider(
        &self,
    ) -> anyhow::Result<ConcreteProvider<ReviveNetwork, Arc<EthereumWallet>>> {
        self.provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<ReviveNetwork, _>(
                    self.rpc_url.as_str(),
                    FallbackGasFiller::default(),
                    ChainIdFiller::new(Some(CHAIN_ID)),
                    NonceFiller::new(self.nonce_manager.clone()),
                    self.wallet.clone(),
                )
                .await
                .context("Failed to construct the provider")
            })
            .await
            .cloned()
    }
}

impl EthereumNode for SubstrateNode {
    fn pre_transactions(&mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.rpc_url
    }

    fn submit_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TxHash>> + '_>> {
        Box::pin(async move {
            let provider = self
                .provider()
                .await
                .context("Failed to create the provider for transaction submission")?;
            let pending_transaction = provider
                .send_transaction(transaction)
                .await
                .context("Failed to submit the transaction through the provider")?;
            Ok(*pending_transaction.tx_hash())
        })
    }

    fn get_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TransactionReceipt>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to create provider for getting the receipt")?
                .get_transaction_receipt(tx_hash)
                .await
                .context("Failed to get the receipt of the transaction")?
                .context("Failed to get the receipt of the transaction")
        })
    }

    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TransactionReceipt>> + '_>> {
        static SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
            std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(500));

        Box::pin(async move {
            let _permit = SEMAPHORE.acquire().await?;

            let receipt = self
                .provider()
                .await
                .context("Failed to create provider for transaction submission")?
                .send_transaction(transaction)
                .await
                .context("Failed to submit transaction to substrate proxy")?
                .get_receipt()
                .await
                .context("Failed to fetch transaction receipt from substrate proxy")?;
            Ok(receipt)
        })
    }

    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<GethTrace>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to create provider for debug tracing")?
                .debug_trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to obtain debug trace from substrate proxy")
        })
    }

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
                .await?
                .try_into_pre_state_frame()?
            {
                PreStateFrame::Diff(diff) => Ok(diff),
                _ => anyhow::bail!("expected a diff mode trace"),
            }
        })
    }

    fn balance_of(
        &self,
        address: Address,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to get the substrate provider")?
                .get_balance(address)
                .await
                .map_err(Into::into)
        })
    }

    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<EIP1186AccountProofResponse>> + '_>> {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to get the substrate provider")?
                .get_proof(address, keys)
                .latest()
                .await
                .map_err(Into::into)
        })
    }

    fn resolver(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Arc<dyn ResolverApi + '_>>> + '_>> {
        Box::pin(async move {
            let id = self.id;
            let provider = self.provider().await?;
            Ok(Arc::new(SubstrateNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
        })
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn subscribe_to_full_blocks_information(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = anyhow::Result<Pin<Box<dyn Stream<Item = MinedBlockInformation>>>>>
                + '_,
        >,
    > {
        Box::pin(async move {
            let provider = self
                .provider()
                .await
                .context("Failed to create the provider for block subscription")?;
            let block_subscription = provider.subscribe_full_blocks();
            let block_stream = block_subscription
                .into_stream()
                .await
                .context("Failed to create the block stream")?;

            let mined_block_information_stream = block_stream.filter_map(|block| async {
                let block = block.ok()?;
                Some(MinedBlockInformation {
                    block_number: block.number(),
                    block_timestamp: block.header.timestamp,
                    mined_gas: block.header.gas_used as _,
                    block_gas_limit: block.header.gas_limit,
                    transaction_hashes: block
                        .transactions
                        .into_hashes()
                        .as_hashes()
                        .expect("Must be hashes")
                        .to_vec(),
                })
            });

            Ok(Box::pin(mined_block_information_stream)
                as Pin<Box<dyn Stream<Item = MinedBlockInformation>>>)
        })
    }
}

pub struct SubstrateNodeResolver {
    id: u32,
    provider: ConcreteProvider<ReviveNetwork, Arc<EthereumWallet>>,
}

impl ResolverApi for SubstrateNodeResolver {
    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn chain_id(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::primitives::ChainId>> + '_>> {
        Box::pin(async move { self.provider.get_chain_id().await.map_err(Into::into) })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
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

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_gas_limit(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u128>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .map(|block| block.header.gas_limit as _)
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_coinbase(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Address>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .map(|block| block.header.beneficiary)
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_difficulty(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_base_fee(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u64>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .and_then(|block| {
                    block
                        .header
                        .base_fee_per_gas
                        .context("Failed to get the base fee per gas")
                })
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_hash(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockHash>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .map(|block| block.header.hash)
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockTimestamp>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the substrate block")?
                .context("Failed to get the substrate block, perhaps the chain has no blocks?")
                .map(|block| block.header.timestamp)
        })
    }

    #[instrument(level = "info", skip_all, fields(substrate_node_id = self.id))]
    fn last_block_number(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockNumber>> + '_>> {
        Box::pin(async move { self.provider.get_block_number().await.map_err(Into::into) })
    }
}

impl Node for SubstrateNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        drop(self.substrate_process.take());
        drop(self.eth_proxy_process.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY));

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.node_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn substrate --version")?
            .wait_with_output()
            .context("Failed to wait for substrate --version")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for SubstrateNode {
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReviveNetwork;

impl Network for ReviveNetwork {
    type TxType = <Ethereum as Network>::TxType;

    type TxEnvelope = <Ethereum as Network>::TxEnvelope;

    type UnsignedTx = <Ethereum as Network>::UnsignedTx;

    type ReceiptEnvelope = <Ethereum as Network>::ReceiptEnvelope;

    type Header = ReviveHeader;

    type TransactionRequest = <Ethereum as Network>::TransactionRequest;

    type TransactionResponse = <Ethereum as Network>::TransactionResponse;

    type ReceiptResponse = <Ethereum as Network>::ReceiptResponse;

    type HeaderResponse = Header<ReviveHeader>;

    type BlockResponse = Block<Transaction<TxEnvelope>, Header<ReviveHeader>>;
}

impl TransactionBuilder<ReviveNetwork> for <Ethereum as Network>::TransactionRequest {
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
        ty: <ReviveNetwork as Network>::TxType,
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

    fn output_tx_type(&self) -> <ReviveNetwork as Network>::TxType {
        <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::output_tx_type(
            self,
        )
    }

    fn output_tx_type_checked(&self) -> Option<<ReviveNetwork as Network>::TxType> {
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
    ) -> alloy::network::BuildResult<<ReviveNetwork as Network>::UnsignedTx, ReviveNetwork> {
        let result = <<Ethereum as Network>::TransactionRequest as TransactionBuilder<Ethereum>>::build_unsigned(
            self,
        );
        match result {
            Ok(unsigned_tx) => Ok(unsigned_tx),
            Err(UnbuiltTransactionError { request, error }) => {
                Err(UnbuiltTransactionError::<ReviveNetwork> {
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

    async fn build<W: alloy::network::NetworkWallet<ReviveNetwork>>(
        self,
        wallet: &W,
    ) -> Result<<ReviveNetwork as Network>::TxEnvelope, TransactionBuilderError<ReviveNetwork>>
    {
        Ok(wallet.sign_request(self).await?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviveHeader {
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

impl BlockHeader for ReviveHeader {
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
    use std::sync::{LazyLock, Mutex};

    use std::fs;

    use super::*;
    use crate::Node;

    fn test_config() -> TestExecutionContext {
        let mut context = TestExecutionContext::default();
        context.kitchensink_configuration.use_kitchensink = true;
        context
    }

    fn new_node() -> (TestExecutionContext, SubstrateNode) {
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

        let context = test_config();
        let mut node = SubstrateNode::new(
            context.kitchensink_configuration.path.clone(),
            SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND,
            &context,
        );
        node.init(context.genesis_configuration.genesis().unwrap().clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    fn shared_state() -> &'static (TestExecutionContext, SubstrateNode) {
        static STATE: LazyLock<(TestExecutionContext, SubstrateNode)> = LazyLock::new(new_node);
        &STATE
    }

    fn shared_node() -> &'static SubstrateNode {
        &shared_state().1
    }

    #[tokio::test]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = shared_state();

        let provider = node.provider().await.expect("Failed to create provider");

        let account_address = context
            .wallet_configuration
            .wallet()
            .default_signer()
            .address();
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

        let context = test_config();
        let mut dummy_node = SubstrateNode::new(
            context.kitchensink_configuration.path.clone(),
            SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND,
            &context,
        );

        // Call `init()`
        dummy_node
            .init(serde_json::from_str(genesis_content).unwrap())
            .expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = dummy_node
            .base_directory
            .join(SubstrateNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents = fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Substrate addresses derived from the Ethereum addresses are in the file
        let first_eth_addr = SubstrateNode::eth_to_substrate_address(
            &"90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap(),
        );
        let second_eth_addr = SubstrateNode::eth_to_substrate_address(
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

        let context = test_config();
        let node = SubstrateNode::new(
            context.kitchensink_configuration.path.clone(),
            SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND,
            &context,
        );

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
            let ss58 = SubstrateNode::eth_to_substrate_address(&eth_addr.parse().unwrap());

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
            let result = SubstrateNode::eth_to_substrate_address(&eth_addr.parse().unwrap());
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    fn version_works() {
        let node = shared_node();

        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected Substrate-node version string, got: {version}"
        );
    }

    #[test]
    fn eth_rpc_version_works() {
        let node = shared_node();

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
        let chain_id = node.resolver().await.unwrap().chain_id().await;

        // Assert
        let chain_id = chain_id.expect("Failed to get the chain id");
        assert_eq!(chain_id, 420_420_420);
    }

    #[tokio::test]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let gas_limit = node
            .resolver()
            .await
            .unwrap()
            .block_gas_limit(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let _ = gas_limit.expect("Failed to get the gas limit");
    }

    #[tokio::test]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let coinbase = node
            .resolver()
            .await
            .unwrap()
            .block_coinbase(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let _ = coinbase.expect("Failed to get the coinbase");
    }

    #[tokio::test]
    async fn can_get_block_difficulty_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_difficulty = node
            .resolver()
            .await
            .unwrap()
            .block_difficulty(BlockNumberOrTag::Latest)
            .await;

        // Assert
        let _ = block_difficulty.expect("Failed to get the block difficulty");
    }

    #[tokio::test]
    async fn can_get_block_hash_from_node() {
        // Arrange
        let node = shared_node();

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
        let node = shared_node();

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
        let node = shared_node();

        // Act
        let block_number = node.resolver().await.unwrap().last_block_number().await;

        // Assert
        let _ = block_number.expect("Failed to get the block number");
    }
}
