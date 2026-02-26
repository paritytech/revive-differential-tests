use std::{
    fs::{File, create_dir_all, remove_dir_all},
    path::{Path, PathBuf},
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
    genesis::Genesis,
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256},
    providers::{
        Provider,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, NonceFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest,
        trace::geth::{
            DiffMode, GethDebugTracingOptions, GethTrace, PreStateConfig, PreStateFrame,
        },
    },
};
use anyhow::Context as _;
use futures::{FutureExt, Stream, StreamExt};
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;
use serde_json::json;
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;

use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::{
    EthereumMinedBlockInformation, MinedBlockInformation, SubstrateMinedBlockInformation,
};
use subxt::{OnlineClient, SubstrateConfig};
use tokio::sync::OnceCell;
use tracing::{instrument, trace};

use crate::{
    Node,
    constants::INITIAL_BALANCE,
    helpers::{Process, ProcessReadinessWaitBehavior},
    provider_utils::{ConcreteProvider, FallbackGasFiller, construct_concurrency_limited_provider},
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The number of blocks that should be cached by the polkadot-omni-node and the eth-rpc.
const NUMBER_OF_CACHED_BLOCKS: u32 = 100_000;

/// A node implementation for the polkadot-omni-node.
#[derive(Debug)]

pub struct PolkadotOmnichainNode {
    /// The id of the node.
    id: u32,

    /// The path of the polkadot-omni-chain node binary.
    polkadot_omnichain_node_binary_path: PathBuf,
    /// The path of the eth-rpc binary.
    eth_rpc_binary_path: PathBuf,
    /// The path of the runtime's WASM that this node will be spawned with.
    chain_spec_path: Option<PathBuf>,
    /// The path of the base directory which contains all of the stored data for this node.
    base_directory_path: PathBuf,
    /// The path of the logs directory which contains all of the stored logs.
    logs_directory_path: PathBuf,

    /// Defines the amount of time to wait before considering that the node start has timed out.
    node_start_timeout: Duration,

    /// The id of the parachain that this node will be spawning.
    parachain_id: Option<usize>,
    /// The block time.
    block_time: Duration,

    /// The node's process.
    polkadot_omnichain_node_process: Option<Process>,
    /// The eth-rpc's process.
    eth_rpc_process: Option<Process>,

    /// The URL of the eth-rpc.
    rpc_url: String,
    /// The wallet object that's used to sign any transaction submitted through this node.
    wallet: Arc<EthereumWallet>,
    /// The nonce manager used to populate nonces for all transactions submitted through this node.
    nonce_manager: CachedNonceManager,
    /// The provider used for all RPC interactions with the RPC of this node.
    provider: OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>,

    /// A boolean that controls if the fallback gas filler should be used or not.
    use_fallback_gas_filler: bool,

    node_logging_level: String,
    eth_rpc_logging_level: String,
}

impl PolkadotOmnichainNode {
    const BASE_DIRECTORY: &str = "polkadot-omni-node";
    const LOGS_DIRECTORY: &str = "logs";

    const POLKADOT_OMNICHAIN_NODE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_RPC_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT: u16 = 9944;
    const BASE_ETH_RPC_PORT: u16 = 8545;

    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<PolkadotOmnichainNodeConfiguration>,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let polkadot_omnichain_node_configuration =
            AsRef::<PolkadotOmnichainNodeConfiguration>::as_ref(&context);
        let working_directory_path =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context).as_path();
        let eth_rpc_path = AsRef::<EthRpcConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path
            .join(Self::BASE_DIRECTORY)
            .join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);

        Self {
            id,
            polkadot_omnichain_node_binary_path: polkadot_omnichain_node_configuration
                .path
                .to_path_buf(),
            eth_rpc_binary_path: eth_rpc_path.to_path_buf(),
            chain_spec_path: polkadot_omnichain_node_configuration
                .chain_spec_path
                .clone(),
            base_directory_path: base_directory,
            logs_directory_path: logs_directory,
            parachain_id: polkadot_omnichain_node_configuration.parachain_id,
            block_time: polkadot_omnichain_node_configuration.block_time,
            polkadot_omnichain_node_process: Default::default(),
            eth_rpc_process: Default::default(),
            rpc_url: Default::default(),
            wallet,
            nonce_manager: Default::default(),
            provider: Default::default(),
            use_fallback_gas_filler,
            node_start_timeout: polkadot_omnichain_node_configuration.start_timeout_ms,
            node_logging_level: polkadot_omnichain_node_configuration.logging_level.clone(),
            eth_rpc_logging_level: AsRef::<EthRpcConfiguration>::as_ref(&context)
                .logging_level
                .clone(),
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        trace!("Removing the various directories");
        let _ = remove_dir_all(self.base_directory_path.as_path());
        let _ = clear_directory(&self.base_directory_path);
        let _ = clear_directory(&self.logs_directory_path);

        trace!("Creating the various directories");
        create_dir_all(&self.base_directory_path)
            .context("Failed to create base directory for polkadot-omni-node node")?;
        create_dir_all(&self.logs_directory_path)
            .context("Failed to create logs directory for polkadot-omni-node node")?;

        let template_chainspec_path = self.base_directory_path.join(Self::CHAIN_SPEC_JSON_FILE);

        let chainspec_json = Self::node_genesis(
            &self.wallet,
            self.chain_spec_path
                .as_ref()
                .context("No runtime path provided")?,
        )
        .context("Failed to prepare the chainspec command")?;

        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)
                .context("Failed to create polkadot-omni-node template chainspec file")?,
            &chainspec_json,
        )
        .context("Failed to write polkadot-omni-node template chainspec JSON")?;

        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        // Error out if the runtime's path or the parachain id are not set which means that the
        // arguments we require were not provided.
        self.chain_spec_path
            .as_ref()
            .context("No WASM path provided for the runtime")?;
        self.parachain_id
            .as_ref()
            .context("No argument provided for the parachain-id")?;

        let polkadot_omnichain_node_rpc_port =
            Self::BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT + self.id as u16;
        let eth_rpc_port = Self::BASE_ETH_RPC_PORT + self.id as u16;

        let chainspec_path = self.base_directory_path.join(Self::CHAIN_SPEC_JSON_FILE);

        self.rpc_url = format!("http://127.0.0.1:{eth_rpc_port}");

        let polkadot_omnichain_node_process = Process::new(
            "node",
            self.logs_directory_path.as_path(),
            self.polkadot_omnichain_node_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--log")
                    .arg(self.node_logging_level.as_str())
                    .arg("--dev-block-time")
                    .arg(self.block_time.as_millis().to_string())
                    .arg("--rpc-port")
                    .arg(polkadot_omnichain_node_rpc_port.to_string())
                    .arg("--base-path")
                    .arg(self.base_directory_path.as_path())
                    .arg("--no-prometheus")
                    .arg("--no-hardware-benchmarks")
                    .arg("--authoring")
                    .arg("slot-based")
                    .arg("--chain")
                    .arg(chainspec_path)
                    .arg("--name")
                    .arg(format!("polkadot-omni-node-{}", self.id))
                    .arg("--rpc-methods")
                    .arg("unsafe")
                    .arg("--rpc-cors")
                    .arg("all")
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--pool-limit")
                    .arg(u32::MAX.to_string())
                    .arg("--pool-kbytes")
                    .arg(u32::MAX.to_string())
                    .arg("--state-pruning")
                    .arg(NUMBER_OF_CACHED_BLOCKS.to_string())
                    .env("RUST_LOG", self.node_logging_level.as_str())
                    .stdout(stdout_file)
                    .stderr(stderr_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: self.node_start_timeout,
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => Ok(line.contains(Self::POLKADOT_OMNICHAIN_NODE_READY_MARKER)),
                    None => Ok(false),
                }),
            },
        );

        match polkadot_omnichain_node_process {
            Ok(process) => self.polkadot_omnichain_node_process = Some(process),
            Err(err) => {
                tracing::error!(
                    ?err,
                    "Failed to start polkadot-omni-node, shutting down gracefully"
                );
                self.shutdown().context(
                    "Failed to gracefully shutdown after polkadot-omni-node start error",
                )?;
                return Err(err);
            }
        }

        let eth_rpc_process = Process::new(
            "eth-rpc",
            self.logs_directory_path.as_path(),
            self.eth_rpc_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--dev")
                    .arg("--rpc-port")
                    .arg(eth_rpc_port.to_string())
                    .arg("--node-rpc-url")
                    .arg(format!("ws://127.0.0.1:{polkadot_omnichain_node_rpc_port}"))
                    .arg("--rpc-max-connections")
                    .arg(u32::MAX.to_string())
                    .arg("--prune")
                    .arg(NUMBER_OF_CACHED_BLOCKS.to_string())
                    .env("RUST_LOG", self.eth_rpc_logging_level.as_str())
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
                tracing::error!(?err, "Failed to start eth-rpc, shutting down gracefully");
                self.shutdown()
                    .context("Failed to gracefully shutdown after eth-rpc start error")?;
                return Err(err);
            }
        }

        Ok(())
    }

    fn eth_to_substrate_address(address: &Address) -> String {
        let eth_bytes = address.0.0;

        let mut padded = [0xEEu8; 32];
        padded[..20].copy_from_slice(&eth_bytes);

        let account_id = AccountId32::from(padded);
        account_id.to_ss58check()
    }

    pub fn eth_rpc_version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.eth_rpc_binary_path)
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
                    self.rpc_url.as_str(),
                    FallbackGasFiller::default()
                        .with_fallback_mechanism(self.use_fallback_gas_filler),
                    ChainIdFiller::default(),
                    NonceFiller::new(self.nonce_manager.clone()),
                    self.wallet.clone(),
                )
                .await
                .context("Failed to construct the provider")
            })
            .await
            .cloned()
    }

    pub fn node_genesis(
        wallet: &EthereumWallet,
        chain_spec_path: &Path,
    ) -> anyhow::Result<serde_json::Value> {
        let unmodified_chainspec_file =
            File::open(chain_spec_path).context("Failed to open the unmodified chainspec file")?;
        let mut chainspec_json =
            serde_json::from_reader::<_, serde_json::Value>(&unmodified_chainspec_file)
                .context("Failed to read the unmodified chainspec JSON")?;

        let existing_chainspec_balances =
            chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
                .as_array_mut()
                .expect("Can't fail");

        for address in NetworkWallet::<Ethereum>::signer_addresses(wallet) {
            let substrate_address = Self::eth_to_substrate_address(&address);
            let balance = INITIAL_BALANCE;
            existing_chainspec_balances.push(json!((substrate_address, balance)));
        }

        Ok(chainspec_json)
    }
}

impl EthereumNode for PolkadotOmnichainNode {
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
        Box::pin(async move {
            self.provider()
                .await
                .context("Failed to create provider for transaction submission")?
                .send_transaction(transaction)
                .await
                .context("Encountered an error when submitting a transaction")?
                .get_receipt()
                .await
                .context("Failed to get the receipt for the transaction")
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
                .context("Failed to obtain debug trace from eth-proxy")
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
                .context("Failed to get the eth-rpc provider")?
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
                .context("Failed to get the eth-rpc provider")?
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
            Ok(Arc::new(PolkadotOmnichainNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
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
            let polkadot_omnichain_node_rpc_port =
                Self::BASE_POLKADOT_OMNICHAIN_NODE_RPC_PORT + self.id as u16;
            let polkadot_omnichain_node_rpc_url =
                format!("ws://127.0.0.1:{polkadot_omnichain_node_rpc_port}");
            let api = OnlineClient::<SubstrateConfig>::from_url(polkadot_omnichain_node_rpc_url)
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
                        ethereum_block_information: EthereumMinedBlockInformation {
                            block_number: revive_block.number(),
                            block_timestamp: revive_block.header.timestamp,
                            mined_gas: revive_block.header.gas_used as _,
                            block_gas_limit: revive_block.header.gas_limit as _,
                            transaction_hashes: revive_block
                                .transactions
                                .into_hashes()
                                .as_hashes()
                                .expect("Must be hashes")
                                .to_vec(),
                        },
                        substrate_block_information: Some(SubstrateMinedBlockInformation {
                            ref_time: block_ref_time,
                            max_ref_time,
                            proof_size: block_proof_size,
                            max_proof_size,
                        }),
                        tx_counts: Default::default(),
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

pub struct PolkadotOmnichainNodeResolver {
    id: u32,
    provider: ConcreteProvider<Ethereum, Arc<EthereumWallet>>,
}

impl ResolverApi for PolkadotOmnichainNodeResolver {
    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn chain_id(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::primitives::ChainId>> + '_>> {
        Box::pin(async move { self.provider.get_chain_id().await.map_err(Into::into) })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
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

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_gas_limit(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u128>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .map(|block| block.header.gas_limit as _)
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_coinbase(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Address>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .map(|block| block.header.beneficiary)
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_difficulty(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .map(|block| U256::from_be_bytes(block.header.mix_hash.0))
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_base_fee(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<u64>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .and_then(|block| {
                    block
                        .header
                        .base_fee_per_gas
                        .context("Failed to get the base fee per gas")
                })
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_hash(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockHash>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .map(|block| block.header.hash)
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockTimestamp>> + '_>> {
        Box::pin(async move {
            self.provider
                .get_block_by_number(number)
                .await
                .context("Failed to get the eth-rpc block")?
                .context("Failed to get the eth-rpc block, perhaps the chain has no blocks?")
                .map(|block| block.header.timestamp)
        })
    }

    #[instrument(level = "info", skip_all, fields(polkadot_omnichain_node_id = self.id))]
    fn last_block_number(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<BlockNumber>> + '_>> {
        Box::pin(async move { self.provider.get_block_number().await.map_err(Into::into) })
    }
}

impl Node for PolkadotOmnichainNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        drop(self.polkadot_omnichain_node_process.take());
        drop(self.eth_rpc_process.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(self.base_directory_path.join("data"));

        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.polkadot_omnichain_node_binary_path)
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

impl Drop for PolkadotOmnichainNode {
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}
