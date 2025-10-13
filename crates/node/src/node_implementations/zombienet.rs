//! # ZombieNode Implementation
//!
//! ## Required Binaries
//! This module requires the following binaries to be compiled and available in your PATH:
//!
//! 1. **polkadot-parachain**:
//!    ```bash
//!    git clone https://github.com/paritytech/polkadot-sdk.git
//!    cd polkadot-sdk
//!    cargo build --release --locked -p polkadot-parachain-bin --bin polkadot-parachain
//!    ```
//!
//! 2. **eth-rpc** (Revive EVM RPC server):
//!    ```bash
//!    git clone https://github.com/paritytech/polkadot-sdk.git
//!    cd polkadot-sdk
//!    cargo build --locked --profile production -p pallet-revive-eth-rpc --bin eth-rpc
//!    ```
//!
//! 3. **polkadot** (for the relay chain):
//!    ```bash
//!    # In polkadot-sdk directory
//!    cargo build --locked --profile testnet --features fast-runtime --bin polkadot --bin polkadot-prepare-worker --bin polkadot-execute-worker
//!    ```
//!
//! Make sure to add the build output directories to your PATH or provide
//! the full paths in your configuration.

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
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256},
    providers::{
        Provider,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};

use anyhow::Context as _;
use futures::{FutureExt, Stream, StreamExt};
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_config::*;
use revive_dt_format::traits::ResolverApi;
use revive_dt_node_interaction::{EthereumNode, MinedBlockInformation};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;
use subxt::{OnlineClient, SubstrateConfig};
use tokio::sync::OnceCell;
use tracing::instrument;
use zombienet_sdk::{LocalFileSystem, NetworkConfigBuilder, NetworkConfigExt};

use crate::{
    Node,
    constants::INITIAL_BALANCE,
    helpers::{Process, ProcessReadinessWaitBehavior},
    provider_utils::{
        ConcreteProvider, FallbackGasFiller, construct_concurrency_limited_provider,
        execute_transaction,
    },
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A Zombienet network where collator is `polkadot-parachain` node with `eth-rpc` [`ZombieNode`]
/// abstracts away the details of managing the zombienet network and provides an interface to
/// interact with the parachain's Ethereum RPC.
#[derive(Debug, Default)]
pub struct ZombienetNode {
    /* Node Identifier */
    id: u32,
    connection_string: String,
    node_rpc_port: Option<u16>,

    /* Directory Paths */
    base_directory: PathBuf,
    logs_directory: PathBuf,

    /* Binary Paths & Timeouts */
    eth_proxy_binary: PathBuf,
    polkadot_parachain_path: PathBuf,

    /* Spawned Processes */
    eth_rpc_process: Option<Process>,

    /* Zombienet Network */
    network_config: Option<zombienet_sdk::NetworkConfig>,
    network: Option<zombienet_sdk::Network<LocalFileSystem>>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,

    provider: OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>,
}

impl ZombienetNode {
    const BASE_DIRECTORY: &str = "zombienet";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const NODE_BASE_RPC_PORT: u16 = 9946;
    const PARACHAIN_ID: u32 = 100;
    const ETH_RPC_BASE_PORT: u16 = 8545;

    const PROXY_LOG_ENV: &str = "info,eth-rpc=debug";

    const ETH_RPC_READY_MARKER: &str = "Running JSON-RPC server";

    const EXPORT_CHAINSPEC_COMMAND: &str = "build-spec";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";

    pub fn new(
        polkadot_parachain_path: PathBuf,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>,
    ) -> Self {
        let eth_proxy_binary = AsRef::<EthRpcConfiguration>::as_ref(&context)
            .path
            .to_owned();
        let working_directory_path = AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path
            .join(Self::BASE_DIRECTORY)
            .join(id.to_string());
        let base_directory = base_directory.canonicalize().unwrap_or(base_directory);
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        Self {
            id,
            base_directory,
            logs_directory,
            wallet,
            polkadot_parachain_path,
            eth_proxy_binary,
            nonce_manager: CachedNonceManager::default(),
            network_config: None,
            network: None,
            eth_rpc_process: None,
            connection_string: String::new(),
            node_rpc_port: None,
            provider: Default::default(),
        }
    }

    fn init(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for zombie node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for zombie node")?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);
        self.prepare_chainspec(template_chainspec_path.clone(), genesis)?;
        let polkadot_parachain_path = self
            .polkadot_parachain_path
            .to_str()
            .context("Invalid polkadot parachain path")?;

        let node_rpc_port = Self::NODE_BASE_RPC_PORT + self.id as u16;

        let network_config = NetworkConfigBuilder::new()
            .with_relaychain(|relay_chain| {
                relay_chain
                    .with_chain("westend-local")
                    .with_default_command("polkadot")
                    .with_node(|node| node.with_name("alice"))
                    .with_node(|node| node.with_name("bob"))
            })
            .with_global_settings(|global_settings| {
                // global_settings.with_base_dir(&self.base_directory)
                global_settings
            })
            .with_parachain(|parachain| {
                parachain
                    .with_id(Self::PARACHAIN_ID)
                    .with_chain_spec_path(template_chainspec_path.to_path_buf())
                    .with_chain("asset-hub-westend-local")
                    .with_collator(|node_config| {
                        node_config
                            .with_name("Collator")
                            .with_command(polkadot_parachain_path)
                            .with_rpc_port(node_rpc_port)
                            .with_args(vec![
                                ("--pool-limit", u32::MAX.to_string().as_str()).into(),
                                ("--pool-kbytes", u32::MAX.to_string().as_str()).into(),
                            ])
                    })
            })
            .build()
            .map_err(|err| anyhow::anyhow!("Failed to build zombienet network config: {err:?}"))?;

        self.node_rpc_port = Some(node_rpc_port);
        self.network_config = Some(network_config);

        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let network_config = self
            .network_config
            .clone()
            .context("Node not initialized, call init() first")?;

        // TODO: Look into the possibility of removing this in the future, perhaps by reintroducing
        // the blocking runtime abstraction and making it available to the entire program so that we
        // don't need to be spawning multiple different runtimes.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let network = rt.block_on(async {
            network_config
                .spawn_native()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to spawn zombienet network: {e:?}"))
        })?;

        tracing::debug!("Zombienet network is up");

        let node_url = format!("ws://localhost:{}", self.node_rpc_port.unwrap());
        let eth_rpc_port = Self::ETH_RPC_BASE_PORT + self.id as u16;

        let eth_rpc_process = Process::new(
            "proxy",
            self.logs_directory.as_path(),
            self.eth_proxy_binary.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--node-rpc-url")
                    .arg(node_url)
                    .arg("--rpc-cors")
                    .arg("all")
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--rpc-port")
                    .arg(eth_rpc_port.to_string())
                    .env("RUST_LOG", Self::PROXY_LOG_ENV)
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(30),
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::ETH_RPC_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );

        match eth_rpc_process {
            Ok(process) => self.eth_rpc_process = Some(process),
            Err(err) => {
                tracing::error!(?err, "Failed to start eth proxy, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth proxy start error")?;
                return Err(err);
            }
        }

        tracing::debug!("eth-rpc is up");

        self.connection_string = format!("http://localhost:{}", eth_rpc_port);
        self.network = Some(network);

        Ok(())
    }

    fn prepare_chainspec(
        &mut self,
        template_chainspec_path: PathBuf,
        mut genesis: Genesis,
    ) -> anyhow::Result<()> {
        let output = Command::new(self.polkadot_parachain_path.as_path())
            .arg(Self::EXPORT_CHAINSPEC_COMMAND)
            .arg("--chain")
            .arg("asset-hub-westend-local")
            .env_remove("RUST_LOG")
            .output()
            .context("Failed to export the chainspec of the chain")?;

        if !output.status.success() {
            anyhow::bail!(
                "Build chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)
            .context("Failed to decode collators chain-spec output as UTF-8")?;
        let mut chainspec_json: JsonValue =
            serde_json::from_str(&content).context("Failed to parse collators chain spec JSON")?;

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

        let writer = std::fs::File::create(&template_chainspec_path)
            .context("Failed to create template chainspec file")?;

        serde_json::to_writer_pretty(writer, &chainspec_json)
            .context("Failed to write template chainspec JSON")?;

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
                let polkadot_address = Self::eth_to_polkadot_address(address);
                let balance = acc.balance.try_into()?;
                vec.push((polkadot_address, balance));
                Ok(vec)
            })
    }

    fn eth_to_polkadot_address(address: &Address) -> String {
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

    async fn provider(&self) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.connection_string.as_str(),
                    FallbackGasFiller::new(u64::MAX, 5_000_000_000, 1_000_000_000),
                    ChainIdFiller::default(), // TODO: use CHAIN_ID constant
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

impl EthereumNode for ZombienetNode {
    fn pre_transactions(&mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.connection_string
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
        transaction: alloy::rpc::types::TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TransactionReceipt>> + '_>> {
        Box::pin(async move {
            let provider = self
                .provider()
                .await
                .context("Failed to create the provider")?;
            execute_transaction(provider, transaction).await
        })
    }

    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::rpc::types::trace::geth::GethTrace>> + '_>>
    {
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to create provider for debug tracing")?
                .debug_trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to obtain debug trace from proxy")
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
                .context("Failed to get the zombie provider")?
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
                .context("Failed to get the zombie provider")?
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

            Ok(Arc::new(ZombieNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
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
        #[subxt::subxt(runtime_metadata_path = "../../assets/revive_metadata.scale")]
        pub mod revive {}

        Box::pin(async move {
            let substrate_rpc_url = format!("ws://127.0.0.1:{}", self.node_rpc_port.unwrap());
            let api = OnlineClient::<SubstrateConfig>::from_url(substrate_rpc_url)
                .await
                .context("Failed to create subxt rpc client")?;
            let provider = self.provider().await.context("Failed to create provider")?;

            let block_stream = api
                .blocks()
                .subscribe_all()
                .await
                .context("Failed to subscribe to blocks")?;

            let mined_block_information_stream = block_stream.filter_map(move |block| {
                let api = api.clone();
                let provider = provider.clone();

                async move {
                    let substrate_block = block.ok()?;
                    let revive_block = provider
                        .get_block_by_number(
                            BlockNumberOrTag::Number(substrate_block.number() as _),
                        )
                        .await
                        .expect("TODO: Remove")
                        .expect("TODO: Remove");

                    let used = api
                        .storage()
                        .at(substrate_block.reference())
                        .fetch_or_default(&revive::storage().system().block_weight())
                        .await
                        .expect("TODO: Remove");

                    let block_ref_time = (used.normal.ref_time as u128)
                        + (used.operational.ref_time as u128)
                        + (used.mandatory.ref_time as u128);
                    let block_proof_size = (used.normal.proof_size as u128)
                        + (used.operational.proof_size as u128)
                        + (used.mandatory.proof_size as u128);

                    let limits = api
                        .constants()
                        .at(&revive::constants().system().block_weights())
                        .expect("TODO: Remove");

                    let max_ref_time = limits.max_block.ref_time;
                    let max_proof_size = limits.max_block.proof_size;

                    Some(MinedBlockInformation {
                        block_number: substrate_block.number() as _,
                        block_timestamp: revive_block.header.timestamp,
                        mined_gas: revive_block.header.gas_used as _,
                        block_gas_limit: revive_block.header.gas_limit as _,
                        transaction_hashes: revive_block
                            .transactions
                            .into_hashes()
                            .as_hashes()
                            .expect("Must be hashes")
                            .to_vec(),
                        ref_time: block_ref_time,
                        max_ref_time,
                        proof_size: block_proof_size,
                        max_proof_size,
                    })
                }
            });

            Ok(Box::pin(mined_block_information_stream)
                as Pin<Box<dyn Stream<Item = MinedBlockInformation>>>)
        })
    }

    fn provider(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::providers::DynProvider<Ethereum>>> + '_>>
    {
        Box::pin(
            self.provider()
                .map(|provider| provider.map(|provider| provider.erased())),
        )
    }
}

pub struct ZombieNodeResolver<F: TxFiller<Ethereum>, P: Provider<Ethereum>> {
    id: u32,
    provider: FillProvider<F, P, Ethereum>,
}

impl<F: TxFiller<Ethereum>, P: Provider<Ethereum>> ResolverApi for ZombieNodeResolver<F, P> {
    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn chain_id(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::primitives::ChainId>> + '_>> {
        Box::pin(async move { self.provider.get_chain_id().await.map_err(Into::into) })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
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

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_gas_limit(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u128>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the block")?
                .context("Failed to get the block, perhaps the chain has no blocks?")
                .map(|block| block.header.gas_limit as _)
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_coinbase(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Address>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the zombie block")?
                .context("Failed to get the zombie block, perhaps the chain has no blocks?")
                .map(|block| block.header.beneficiary)
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_difficulty(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the zombie block")?
                .context("Failed to get the zombie block, perhaps the chain has no blocks?")
                .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_base_fee(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u64>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the zombie block")?
                .context("Failed to get the zombie block, perhaps the chain has no blocks?")
                .and_then(|block| {
                    block
                        .header
                        .base_fee_per_gas
                        .context("Failed to get the base fee per gas")
                })
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_hash(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockHash>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the zombie block")?
                .context("Failed to get the zombie block, perhaps the chain has no blocks?")
                .map(|block| block.header.hash)
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockTimestamp>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the zombie block")?
                .context("Failed to get the zombie block, perhaps the chain has no blocks?")
                .map(|block| block.header.timestamp)
        })
    }

    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
    fn last_block_number(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockNumber>> + '_>> {
        Box::pin(async move { self.provider.get_block_number().await.map_err(Into::into) })
    }
}

impl Node for ZombienetNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        // Kill the eth_rpc process
        drop(self.eth_rpc_process.take());

        // Destroy the network
        if let Some(network) = self.network.take() {
            // Handle network cleanup here
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Err(e) = network.destroy().await {
                        tracing::warn!("Failed to destroy zombienet network: {e:?}");
                    }
                })
            });
        }

        // Remove the database directory
        if let Err(e) = remove_dir_all(self.base_directory.join(Self::DATA_DIRECTORY)) {
            tracing::warn!("Failed to remove database directory: {e:?}");
        }

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.polkadot_parachain_path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed execute --version")?
            .wait_with_output()
            .context("Failed to wait --version")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for ZombienetNode {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;

    use crate::node_implementations::zombienet::tests::utils::shared_node;

    use super::*;

    mod utils {
        use super::*;

        use std::sync::Arc;
        use tokio::sync::OnceCell;

        pub fn test_config() -> TestExecutionContext {
            TestExecutionContext::default()
        }

        pub async fn new_node() -> (TestExecutionContext, ZombienetNode) {
            let context = test_config();
            let mut node = ZombienetNode::new(
                context.polkadot_parachain_configuration.path.clone(),
                &context,
            );
            let genesis = context.genesis_configuration.genesis().unwrap().clone();
            node.init(genesis).unwrap();

            // Run spawn_process in a blocking thread
            let node = tokio::task::spawn_blocking(move || {
                node.spawn_process().unwrap();
                node
            })
            .await
            .expect("Failed to spawn process");

            (context, node)
        }

        pub async fn shared_state() -> &'static (TestExecutionContext, Arc<ZombienetNode>) {
            static NODE: OnceCell<(TestExecutionContext, Arc<ZombienetNode>)> =
                OnceCell::const_new();

            NODE.get_or_init(|| async {
                let (context, node) = new_node().await;
                (context, Arc::new(node))
            })
            .await
        }

        pub async fn shared_node() -> &'static Arc<ZombienetNode> {
            &shared_state().await.1
        }
    }
    use utils::{new_node, test_config};

    #[tokio::test]
    #[ignore = "Ignored for the time being"]
    async fn test_transfer_transaction_should_return_receipt() {
        let (ctx, node) = new_node().await;

        let provider = node.provider().await.expect("Failed to create provider");
        let account_address = ctx.wallet_configuration.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        let receipt = provider.send_transaction(transaction).await;
        let _ = receipt
            .expect("Failed to send the transfer transaction")
            .get_receipt()
            .await
            .expect("Failed to get the receipt for the transfer");
    }

    #[tokio::test]
    async fn test_init_generates_chainspec_with_balances() {
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
        let mut node = ZombienetNode::new(
            context.polkadot_parachain_configuration.path.clone(),
            &context,
        );

        // Call `init()`
        node.init(serde_json::from_str(genesis_content).unwrap())
            .expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = node
            .base_directory
            .join(ZombienetNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents =
            std::fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Polkadot addresses derived from the Ethereum addresses are in the file
        let first_eth_addr = ZombienetNode::eth_to_polkadot_address(
            &"90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap(),
        );
        let second_eth_addr = ZombienetNode::eth_to_polkadot_address(
            &"Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2".parse().unwrap(),
        );

        assert!(
            contents.contains(&first_eth_addr),
            "Chainspec should contain Polkadot address for first Ethereum account"
        );
        assert!(
            contents.contains(&second_eth_addr),
            "Chainspec should contain Polkadot address for second Ethereum account"
        );
    }

    #[tokio::test]
    async fn test_parse_genesis_alloc() {
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
        let node = ZombienetNode::new(
            context.polkadot_parachain_configuration.path.clone(),
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
    fn print_eth_to_polkadot_mappings() {
        let eth_addresses = vec![
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
            "0xffffffffffffffffffffffffffffffffffffffff",
            "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
        ];

        for eth_addr in eth_addresses {
            let ss58 = ZombienetNode::eth_to_polkadot_address(&eth_addr.parse().unwrap());

            println!("Ethereum: {eth_addr} -> Polkadot SS58: {ss58}");
        }
    }

    #[test]
    fn test_eth_to_polkadot_address() {
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
            let result = ZombienetNode::eth_to_polkadot_address(&eth_addr.parse().unwrap());
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    fn eth_rpc_version_works() {
        // Arrange
        let context = test_config();
        let node = ZombienetNode::new(
            context.polkadot_parachain_configuration.path.clone(),
            &context,
        );

        // Act
        let version = node.eth_rpc_version().unwrap();

        // Assert
        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }

    #[test]
    fn version_works() {
        // Arrange
        let context = test_config();
        let node = ZombienetNode::new(
            context.polkadot_parachain_configuration.path.clone(),
            &context,
        );

        // Act
        let version = node.version().unwrap();

        // Assert
        assert!(
            version.starts_with("polkadot-parachain"),
            "Expected Polkadot-parachain version string, got: {version}"
        );
    }

    #[tokio::test]
    #[ignore = "Ignored since they take a long time to run"]
    async fn get_chain_id_from_node_should_succeed() {
        // Arrange
        let node = shared_node().await;

        // Act
        let chain_id = node
            .resolver()
            .await
            .expect("Failed to create resolver")
            .chain_id()
            .await
            .expect("Failed to get chain id");

        // Assert
        assert!(chain_id > 0, "Chain ID should be greater than zero");
    }

    #[tokio::test]
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let node = shared_node().await;

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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let node = shared_node().await;

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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_block_difficulty_from_node() {
        // Arrange
        let node = shared_node().await;

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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_block_hash_from_node() {
        // Arrange
        let node = shared_node().await;

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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_block_timestamp_from_node() {
        // Arrange
        let node = shared_node().await;

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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_block_number_from_node() {
        // Arrange
        let node = shared_node().await;

        // Act
        let block_number = node.resolver().await.unwrap().last_block_number().await;

        // Assert
        let _ = block_number.expect("Failed to get the block number");
    }
}
