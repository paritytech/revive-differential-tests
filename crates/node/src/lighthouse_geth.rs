//! This module implements an Ethereum network that's identical to mainnet in all ways where Geth is
//! used as the execution engine and doesn't perform any kind of consensus, which now happens on the
//! beacon chain and between the beacon nodes. We're using lighthouse as the consensus node in this
//! case with 12 second block slots which is identical to Ethereum mainnet.
//!
//! Ths implementation uses the Kurtosis tool to spawn the various nodes that are needed which means
//! that this tool is now a requirement that needs to be installed in order for this target to be
//! used. Additionally, the Kurtosis tool uses Docker and therefore docker is a another dependency
//! that the tool has.

use std::{
    collections::BTreeMap,
    fs::{File, create_dir_all},
    io::Read,
    ops::ControlFlow,
    path::PathBuf,
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy::{
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{
        Address, BlockHash, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256, address,
    },
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
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::serde_as;
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
/// Assumes dev mode and IPC only (`P2P`, `http` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
#[allow(clippy::type_complexity)]
pub struct LighthouseGethNode {
    /* Node Identifier */
    id: u32,
    connection_string: String,
    enclave_name: String,

    /* Directory Paths */
    base_directory: PathBuf,
    logs_directory: PathBuf,

    /* File Paths */
    config_file_path: PathBuf,

    /* Binary Paths & Timeouts */
    kurtosis_binary_path: PathBuf,

    /* Spawned Processes */
    process: Option<Process>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    chain_id_filler: ChainIdFiller,
}

impl LighthouseGethNode {
    const BASE_DIRECTORY: &str = "lighthouse";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE_NAME: &str = "geth.ipc";
    const CONFIG_FILE_NAME: &str = "config.yaml";

    const TRANSACTION_INDEXING_ERROR: &str = "transaction indexing is in progress";
    const TRANSACTION_TRACING_ERROR: &str = "historical state not available in path scheme yet";

    const RECEIPT_POLLING_DURATION: Duration = Duration::from_secs(5 * 60);
    const TRACE_POLLING_DURATION: Duration = Duration::from_secs(60);

    const VALIDATOR_MNEMONIC: &str = "giant issue aisle success illegal bike spike question tent bar rely arctic volcano long crawl hungry vocal artwork sniff fantasy very lucky have athlete";

    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<KurtosisConfiguration>
        + Clone,
    ) -> Self {
        let working_directory_configuration =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let wallet_configuration = AsRef::<WalletConfiguration>::as_ref(&context);
        let kurtosis_configuration = AsRef::<KurtosisConfiguration>::as_ref(&context);

        let geth_directory = working_directory_configuration
            .as_path()
            .join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        let wallet = wallet_configuration.wallet();

        Self {
            /* Node Identifier */
            id,
            connection_string: base_directory
                .join(Self::IPC_FILE_NAME)
                .display()
                .to_string(),
            enclave_name: format!(
                "enclave-{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Must not fail")
                    .as_nanos(),
                id
            ),

            /* File Paths */
            config_file_path: base_directory.join(Self::CONFIG_FILE_NAME),

            /* Directory Paths */
            logs_directory: base_directory.join(Self::LOGS_DIRECTORY),
            base_directory,

            /* Binary Paths & Timeouts */
            kurtosis_binary_path: kurtosis_configuration.path.clone(),

            /* Spawned Processes */
            process: None,

            /* Provider Related Fields */
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            chain_id_filler: Default::default(),
        }
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        self.init_directories()
            .context("Failed to initialize the directories of the Lighthouse Geth node.")?;
        self.init_kurtosis_config_file()
            .context("Failed to write the config file to the FS")?;

        Ok(self)
    }

    fn init_directories(&self) -> anyhow::Result<()> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for geth node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for geth node")?;

        Ok(())
    }

    fn init_kurtosis_config_file(&self) -> anyhow::Result<()> {
        let config = KurtosisNetworkConfig {
            participants: vec![ParticipantParameters {
                execution_layer_type: ExecutionLayerType::Geth,
                consensus_layer_type: ConsensusLayerType::Lighthouse,
                execution_layer_extra_parameters: vec![
                    "--nodiscover".to_string(),
                    "--cache=4096".to_string(),
                    "--txpool.globalslots=100000".to_string(),
                    "--txpool.globalqueue=100000".to_string(),
                    "--txpool.accountslots=128".to_string(),
                    "--txpool.accountqueue=1024".to_string(),
                    "--http.api=admin,engine,net,eth,web3,debug,txpool".to_string(),
                    "--http.addr=0.0.0.0".to_string(),
                    "--ws".to_string(),
                    "--ws.addr=0.0.0.0".to_string(),
                    "--ws.port=8546".to_string(),
                    "--ws.api=eth,net,web3,txpool,engine".to_string(),
                    "--ws.origins=*".to_string(),
                ],
                consensus_layer_extra_parameters: vec![
                    "--disable-deposit-contract-sync".to_string(),
                ],
            }],
            network_parameters: NetworkParameters {
                preset: NetworkPreset::Mainnet,
                seconds_per_slot: 12,
                network_id: 420420420,
                deposit_contract_address: address!("0x00000000219ab540356cBB839Cbe05303d7705Fa"),
                altair_fork_epoch: 0,
                bellatrix_fork_epoch: 0,
                capella_fork_epoch: 0,
                deneb_fork_epoch: 0,
                electra_fork_epoch: 0,
                preregistered_validator_keys_mnemonic: Self::VALIDATOR_MNEMONIC.to_string(),
                num_validator_keys_per_node: 64,
                genesis_delay: 10,
                prefunded_accounts: {
                    let map = NetworkWallet::<Ethereum>::signer_addresses(&self.wallet)
                        .map(|address| {
                            (
                                address,
                                GenesisAccount::default()
                                    .with_balance(INITIAL_BALANCE.try_into().unwrap()),
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    serde_json::to_string(&map).unwrap()
                },
            },
            wait_for_finalization: false,
            port_publisher: Some(PortPublisherParameters {
                execution_layer_port_publisher_parameters: Some(
                    PortPublisherSingleItemParameters {
                        enabled: Some(true),
                        public_port_start: Some(32000 + self.id as u16 * 1000),
                    },
                ),
                consensus_layer_port_publisher_parameters: Default::default(),
            }),
        };

        let file = File::create(self.config_file_path.as_path())
            .context("Failed to open the config yaml file")?;
        serde_yaml_ng::to_writer(file, &config)
            .context("Failed to write the config to the yaml file")?;

        Ok(())
    }

    /// Spawn the go-ethereum node child process.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let process = Process::new(
            None,
            self.logs_directory.as_path(),
            self.kurtosis_binary_path.as_path(),
            |command, stdout, stderr| {
                command
                    .arg("run")
                    .arg("--enclave")
                    .arg(self.enclave_name.as_str())
                    .arg("github.com/ethpandaops/ethereum-package")
                    .arg("--args-file")
                    .arg(self.config_file_path.as_path())
                    .stdout(stdout)
                    .stderr(stderr);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: Duration::from_secs(15 * 60),
                check_function: Box::new(|stdout, stderr| {
                    for line in [stdout, stderr].iter().flatten() {
                        if line.to_lowercase().contains("error encountered") {
                            anyhow::bail!("Encountered an error when starting Kurtosis")
                        } else if line.contains("RUNNING") {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }),
            },
        )
        .inspect_err(|err| {
            tracing::error!(?err, "Failed to spawn Kurtosis");
            self.shutdown().expect("Failed to shutdown kurtosis");
        })?;
        self.process = Some(process);

        let child = Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("inspect")
            .arg(self.enclave_name.as_str())
            .stdout(Stdio::piped())
            .spawn()
            .context("Failed to spawn the kurtosis enclave inspect process")?;

        let stdout = {
            let mut stdout = String::default();
            child
                .stdout
                .expect("Should be piped")
                .read_to_string(&mut stdout)
                .context("Failed to read stdout of kurtosis inspect to string")?;
            stdout
        };

        self.connection_string = stdout
            .split("el-1-geth-lighthouse")
            .nth(1)
            .and_then(|str| str.split("ws").nth(1))
            .and_then(|str| str.split("->").nth(1))
            .and_then(|str| str.split("\n").next())
            .map(|str| format!("ws://{}", str.trim()))
            .context("Failed to find the WS connection string of Kurtosis")?;

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
            .context("Failed to create the provider for Kurtosis")
    }
}

impl EthereumNode for LighthouseGethNode {
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
                .inspect_err(|err| {
                    tracing::error!(
                        %err,
                        "Encountered an error when submitting the transaction"
                    )
                })
                .context("Failed to submit transaction to geth node")?;
            let transaction_hash = *pending_transaction.tx_hash();

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
            Ok(Arc::new(LighthouseGethNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
        })
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }
}

pub struct LighthouseGethNodeResolver<F: TxFiller<Ethereum>, P: Provider<Ethereum>> {
    id: u32,
    provider: FillProvider<F, P, Ethereum>,
}

impl<F: TxFiller<Ethereum>, P: Provider<Ethereum>> ResolverApi
    for LighthouseGethNodeResolver<F, P>
{
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

impl Node for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn shutdown(&mut self) -> anyhow::Result<()> {
        if !Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("rm")
            .arg("-f")
            .arg(self.enclave_name.as_str())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn the enclave kill command")
            .wait()
            .expect("Failed to wait for the enclave kill command")
            .success()
        {
            panic!("Failed to shut down the enclave {}", self.enclave_name)
        }

        drop(self.process.take());

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.kurtosis_binary_path)
            .arg("version")
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

impl Drop for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KurtosisNetworkConfig {
    pub participants: Vec<ParticipantParameters>,

    #[serde(rename = "network_params")]
    pub network_parameters: NetworkParameters,

    pub wait_for_finalization: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_publisher: Option<PortPublisherParameters>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParticipantParameters {
    #[serde(rename = "el_type")]
    pub execution_layer_type: ExecutionLayerType,

    #[serde(rename = "el_extra_params")]
    pub execution_layer_extra_parameters: Vec<String>,

    #[serde(rename = "cl_type")]
    pub consensus_layer_type: ConsensusLayerType,

    #[serde(rename = "cl_extra_params")]
    pub consensus_layer_extra_parameters: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExecutionLayerType {
    Geth,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ConsensusLayerType {
    Lighthouse,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct NetworkParameters {
    pub preset: NetworkPreset,

    pub seconds_per_slot: u64,

    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub network_id: u64,

    pub deposit_contract_address: Address,

    pub altair_fork_epoch: u64,
    pub bellatrix_fork_epoch: u64,
    pub capella_fork_epoch: u64,
    pub deneb_fork_epoch: u64,
    pub electra_fork_epoch: u64,

    pub preregistered_validator_keys_mnemonic: String,

    pub num_validator_keys_per_node: u64,

    pub genesis_delay: u64,

    pub prefunded_accounts: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NetworkPreset {
    Mainnet,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PortPublisherParameters {
    #[serde(rename = "el", skip_serializing_if = "Option::is_none")]
    pub execution_layer_port_publisher_parameters: Option<PortPublisherSingleItemParameters>,

    #[serde(rename = "cl", skip_serializing_if = "Option::is_none")]
    pub consensus_layer_port_publisher_parameters: Option<PortPublisherSingleItemParameters>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct PortPublisherSingleItemParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_port_start: Option<u16>,
}

/// Custom serializer/deserializer for u128 values as 0x-prefixed hex strings
pub struct HexPrefixedU128;

impl serde_with::SerializeAs<u128> for HexPrefixedU128 {
    fn serialize_as<S>(source: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_string = format!("0x{source:x}");
        serializer.serialize_str(&hex_string)
    }
}

impl<'de> serde_with::DeserializeAs<'de, u128> for HexPrefixedU128 {
    fn deserialize_as<D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_string = String::deserialize(deserializer)?;
        if let Some(hex_part) = hex_string.strip_prefix("0x") {
            u128::from_str_radix(hex_part, 16).map_err(serde::de::Error::custom)
        } else {
            Err(serde::de::Error::custom("Expected 0x-prefixed hex string"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn test_config() -> TestExecutionContext {
        TestExecutionContext::default()
    }

    fn new_node() -> (TestExecutionContext, LighthouseGethNode) {
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
        let mut node = LighthouseGethNode::new(&context);
        node.init(context.genesis_configuration.genesis().unwrap().clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    #[tokio::test]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = new_node();

        let account_address = context
            .wallet_configuration
            .wallet()
            .default_signer()
            .address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let receipt = node.execute_transaction(transaction).await;

        // Assert
        let _ = receipt.expect("Failed to send the transfer transaction");
    }

    #[test]
    fn version_works() {
        // Arrange
        let (_context, node) = new_node();

        // Act
        let version = node.version();

        // Assert
        let version = version.expect("Failed to get the version");
        assert!(
            version.starts_with("CLI Version"),
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
        let _ = gas_limit.expect("Failed to get the gas limit");
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
        let _ = coinbase.expect("Failed to get the coinbase");
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
        let _ = block_difficulty.expect("Failed to get the block difficulty");
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
        let _ = block_number.expect("Failed to get the block number");
    }
}
