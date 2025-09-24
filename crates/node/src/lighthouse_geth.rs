//! This module implements an Ethereum network that's identical to mainnet in all ways where Geth is
//! used as the execution engine and doesn't perform any kind of consensus, which now happens on the
//! beacon chain and between the beacon nodes. We're using lighthouse as the consensus node in this
//! case with 12 second block slots which is identical to Ethereum mainnet.

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
    hex,
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
/// Assumes dev mode and IPC only (`P2P`, `http` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
#[allow(clippy::type_complexity)]
pub struct LighthouseGethNode {
    /* Node Identifier */
    id: u32,
    connection_string: String,

    /* Ports */
    execution_layer_auth_rpc_port: u16,
    beacon_node_listening_port: u16,
    beacon_node_http_port: u16,
    validator_node_http_port: u16,

    /* Directory Paths */
    base_directory: PathBuf,
    logs_directory: PathBuf,
    execution_layer_data_directory: PathBuf,

    /* File Paths */
    shared_secret_file_path: PathBuf,
    execution_layer_genesis_json_file_path: PathBuf,
    consensus_layer_genesis_json_file_path: PathBuf,
    mnemonic_text_file_path: PathBuf,
    mnemonic_yaml_file_path: PathBuf,
    consensus_layer_config_file_path: PathBuf,
    deposit_contract_block_file_path: PathBuf,
    validator_api_token_file_path: PathBuf,
    validators_json_file_path: PathBuf,

    /* Binary Paths & Timeouts */
    geth_binary_path: PathBuf,
    geth_binary_start_timeout: Duration,
    lighthouse_binary_path: PathBuf,
    lighthouse_binary_start_timeout: Duration,

    /* Spawned Processes */
    geth_execution_layer_process: Option<Process>,
    lighthouse_beacon_node_process: Option<Process>,
    lighthouse_validator_node_process: Option<Process>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    chain_id_filler: ChainIdFiller,
}

impl LighthouseGethNode {
    const BASE_DIRECTORY: &str = "lighthouse";
    const EXECUTION_LAYER_DATA_DIRECTORY: &str = "el_data";
    const LOGS_DIRECTORY: &str = "logs";

    const IPC_FILE_NAME: &str = "geth.ipc";
    const SHARED_SECRET_FILE_NAME: &str = "jwt.hex";
    const EXECUTION_LAYER_GENESIS_JSON_FILE_NAME: &str = "genesis.json";
    const CONSENSUS_LAYER_GENESIS_JSON_FILE_NAME: &str = "genesis.ssz";
    const MNEMONIC_YAML_FILE_NAME: &str = "mnemonic.yaml";
    const MNEMONIC_TEXT_FILE_NAME: &str = "mnemonic.txt";
    const CONSENSUS_LAYER_CONFIGURATION_FILE_NAME: &str = "config.yaml";
    const DEPOSIT_CONTRACT_BLOCK_FILE_NAME: &str = "deposit_contract_block.txt";
    const VALIDATORS_JSON_FILE_NAME: &str = "validators.json";
    const VALIDATOR_API_TOKEN_FILE_NAME: &str = "api-token.txt";

    const GETH_READY_MARKER: &str = "IPC endpoint opened";
    const LIGHTHOUSE_BEACON_NODE_READY_MARKER: &str = "HTTP server started";
    const ERROR_MARKER: &str = "Fatal:";

    const TRANSACTION_INDEXING_ERROR: &str = "transaction indexing is in progress";
    const TRANSACTION_TRACING_ERROR: &str = "historical state not available in path scheme yet";

    const RECEIPT_POLLING_DURATION: Duration = Duration::from_secs(5 * 60);
    const TRACE_POLLING_DURATION: Duration = Duration::from_secs(60);

    /// A shared secret key used for communication between the execution layer and the consensus layer.
    const SHARED_SECRET_KEY: [u8; 32] =
        hex!("fa0d813b5c3e9684f41ec77662c85eeaebbdde3b1d22d4f64439d9fdde3f517e");

    /// The mnemonic phrase of the validator node that will be spawned.
    const MNEMONIC_PHRASE: &str = "test test test test test test test test test test test junk";

    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<GethConfiguration>
        + AsRef<LighthouseConfiguration>
        + Clone,
    ) -> Self {
        let working_directory_configuration =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let wallet_configuration = AsRef::<WalletConfiguration>::as_ref(&context);
        let geth_configuration = AsRef::<GethConfiguration>::as_ref(&context);
        let lighthouse_configuration = AsRef::<LighthouseConfiguration>::as_ref(&context);

        let geth_directory = working_directory_configuration
            .as_path()
            .join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        let shared_secret_file_path = base_directory.join(Self::SHARED_SECRET_FILE_NAME);

        let wallet = wallet_configuration.wallet();

        Self {
            /* Node Identifier */
            id,
            connection_string: base_directory
                .join(Self::IPC_FILE_NAME)
                .display()
                .to_string(),

            /* Ports */
            execution_layer_auth_rpc_port: 8551u16 + id as u16,
            beacon_node_listening_port: 9000u16 + id as u16,
            beacon_node_http_port: 5052u16 + id as u16,
            validator_node_http_port: 5062u16 + id as u16,

            /* File Paths */
            shared_secret_file_path,
            execution_layer_genesis_json_file_path: base_directory
                .join(Self::EXECUTION_LAYER_GENESIS_JSON_FILE_NAME),
            consensus_layer_genesis_json_file_path: base_directory
                .join(Self::CONSENSUS_LAYER_GENESIS_JSON_FILE_NAME),
            mnemonic_text_file_path: base_directory.join(Self::MNEMONIC_TEXT_FILE_NAME),
            mnemonic_yaml_file_path: base_directory.join(Self::MNEMONIC_YAML_FILE_NAME),
            consensus_layer_config_file_path: base_directory
                .join(Self::CONSENSUS_LAYER_CONFIGURATION_FILE_NAME),
            deposit_contract_block_file_path: base_directory
                .join(Self::DEPOSIT_CONTRACT_BLOCK_FILE_NAME),
            validators_json_file_path: base_directory.join(Self::VALIDATORS_JSON_FILE_NAME),
            validator_api_token_file_path: base_directory.join(Self::VALIDATOR_API_TOKEN_FILE_NAME),

            /* Directory Paths */
            execution_layer_data_directory: base_directory
                .join(Self::EXECUTION_LAYER_DATA_DIRECTORY),
            logs_directory: base_directory.join(Self::LOGS_DIRECTORY),
            base_directory,

            /* Binary Paths & Timeouts */
            geth_binary_path: geth_configuration.path.clone(),
            geth_binary_start_timeout: geth_configuration.start_timeout_ms,
            lighthouse_binary_path: lighthouse_configuration.path.clone(),
            lighthouse_binary_start_timeout: lighthouse_configuration.start_timeout_ms,

            /* Spawned Processes */
            geth_execution_layer_process: None,
            lighthouse_beacon_node_process: None,
            lighthouse_validator_node_process: None,

            /* Provider Related Fields */
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            chain_id_filler: Default::default(),
        }
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn init(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        self.init_directories()
            .context("Failed to initialize the directories of the Lighthouse Geth node.")?;
        self.init_execution_layer_genesis(genesis)
            .context("Failed to initialize the genesis of the Geth lighthouse node")?;
        self.init_execution_layer_data_directory()
            .context("Failed to initialize the data directory of the execution layer")?;
        self.init_execution_layer_consensus_layer_shared_secret()
            .context("Failed to initialize the shared secret")?;
        self.init_mnemonic()
            .context("Failed to initialize the mnemonic files of the consensus layer")?;
        self.init_consensus_layer_configuration()
            .context("Failed to initialize the consensus layer configuration file")?;
        self.init_consensus_layer_genesis()
            .context("Failed to initialize the consensus layer genesis")?;
        self.init_deposit_contract_block_file()
            .context("Failed to initialize the deposit contract block file")?;
        self.init_lighthouse_validation_creation()
            .context("Failed to create the consensus layer validator")?;

        Ok(self)
    }

    fn init_directories(&mut self) -> anyhow::Result<()> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for geth node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for geth node")?;

        Ok(())
    }

    fn init_execution_layer_genesis(&mut self, mut genesis: Genesis) -> anyhow::Result<()> {
        for signer_address in NetworkWallet::<Ethereum>::signer_addresses(&self.wallet) {
            // Note, the use of the entry API here means that we only modify the entries for any
            // account that is not in the `alloc` field of the genesis state.
            genesis
                .alloc
                .entry(signer_address)
                .or_insert(GenesisAccount::default().with_balance(U256::from(INITIAL_BALANCE)));
        }

        let file = File::create(self.execution_layer_genesis_json_file_path.as_path())
            .context("Failed to create the Geth genesis file")?;
        serde_json::to_writer(file, &genesis)
            .context("Failed to serialize geth genesis JSON to file")?;

        Ok(())
    }

    fn init_execution_layer_data_directory(&mut self) -> anyhow::Result<()> {
        let mut child = Command::new(&self.geth_binary_path)
            .arg("--state.scheme")
            .arg("hash")
            .arg("init")
            .arg("--datadir")
            .arg(&self.execution_layer_data_directory)
            .arg(self.execution_layer_genesis_json_file_path.as_path())
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

        Ok(())
    }

    fn init_execution_layer_consensus_layer_shared_secret(&mut self) -> anyhow::Result<()> {
        std::fs::write(
            self.shared_secret_file_path.as_path(),
            hex::encode(Self::SHARED_SECRET_KEY),
        )
        .context("Failed to write shared secret key to the appropriate file.")?;

        Ok(())
    }

    fn init_mnemonic(&mut self) -> anyhow::Result<()> {
        // Write the mnemonic text file.
        std::fs::write(
            self.mnemonic_text_file_path.as_path(),
            Self::MNEMONIC_PHRASE,
        )
        .context("Failed to write the mnemonic text file")?;

        // Write the mnemonic YAML file.
        std::fs::write(
            self.mnemonic_yaml_file_path.as_path(),
            format!("- mnemonic: \"{}\"\n  count: 1", Self::MNEMONIC_PHRASE),
        )
        .context("Failed to write the mnemonic text file")?;

        Ok(())
    }

    fn init_consensus_layer_configuration(&mut self) -> anyhow::Result<()> {
        const DEFAULT_CONFIGURATION: &str = include_str!("../../../assets/config.yaml");

        std::fs::write(
            self.consensus_layer_config_file_path.as_path(),
            DEFAULT_CONFIGURATION,
        )
        .context("Failed to write the consensus layer configuration file")?;

        Ok(())
    }

    fn init_consensus_layer_genesis(&mut self) -> anyhow::Result<()> {
        // TODO: should the path of this be configurable?
        let mut child = Command::new("eth2-testnet-genesis")
            .arg("electra")
            .arg("--config")
            .arg(self.consensus_layer_config_file_path.as_path())
            .arg("--eth1-config")
            .arg(self.execution_layer_genesis_json_file_path.as_path())
            .arg("--mnemonics")
            .arg(self.mnemonic_yaml_file_path.as_path())
            .arg("--state-output")
            .arg(self.consensus_layer_genesis_json_file_path.as_path())
            .arg("--tranches-dir")
            .arg(self.base_directory.join("tranches"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn the eth2-testnet-genesis command")?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)
            .context("Failed to read the eth2-testnet-genesis stderr")?;

        if !child
            .wait()
            .context("Failed waiting for eth2-testnet-genesis process to finish")?
            .success()
        {
            anyhow::bail!("Failed when calling 'eth2-testnet-genesis': {stderr}");
        }

        Ok(())
    }

    fn init_deposit_contract_block_file(&mut self) -> anyhow::Result<()> {
        std::fs::write(
            self.deposit_contract_block_file_path.as_path(),
            "0x0000000000000000000000000000000000000000",
        )?;
        Ok(())
    }

    fn init_lighthouse_validation_creation(&mut self) -> anyhow::Result<()> {
        // TODO: should the path of this be configurable?
        let mut child = Command::new(self.lighthouse_binary_path.as_path())
            .arg("validator_manager")
            .arg("create")
            .arg("--mnemonic-path")
            .arg(self.mnemonic_text_file_path.as_path())
            .arg("--count")
            .arg("1")
            .arg("--suggested-fee-recipient")
            .arg("0x0000000000000000000000000000000000000000")
            .arg("--eth1-withdrawal-address")
            .arg("0x0000000000000000000000000000000000000000")
            .arg("--testnet-dir")
            .arg(self.base_directory.as_path())
            .arg("--output-path")
            .arg(self.base_directory.as_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn the lighthouse validator_manager create command")?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)
            .context("Failed to read the lighthouse validator_manager create stderr")?;

        if !child
            .wait()
            .context("Failed waiting for lighthouse validator_manager create process to finish")?
            .success()
        {
            anyhow::bail!("Failed when calling 'lighthouse validator_manager create': {stderr}");
        }

        Ok(())
    }

    /// Spawn the go-ethereum node child process.
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        self.geth_execution_layer_process = self
            .spawn_geth_process()
            .context("Failed to spawn geth process")
            .inspect_err(|err| {
                tracing::error!(?err, "Failed to start the geth process");
                self.shutdown()
                    .expect("Failed to shutdown the lighthouse geth node");
            })
            .map(Into::into)?;
        self.lighthouse_beacon_node_process = self
            .spawn_lighthouse_beacon_node_process()
            .context("Failed to spawn the lighthouse beacon node")
            .inspect_err(|err| {
                tracing::error!(?err, "Failed to start the light house beacon process");
                self.shutdown()
                    .expect("Failed to shutdown the lighthouse light house beacon node");
            })
            .map(Into::into)?;
        self.lighthouse_validator_node_process = self
            .spawn_lighthouse_validator_node_process()
            .context("Failed to spawn the lighthouse validator node")
            .inspect_err(|err| {
                tracing::error!(?err, "Failed to start the light house validator process");
                self.shutdown()
                    .expect("Failed to shutdown the lighthouse light house validator node");
            })
            .map(Into::into)?;

        self.import_validator_keys().inspect_err(|err| {
            tracing::error!(?err, "Failed to import the validator keys");
            self.shutdown()
                .expect("Failed to shutdown the lighthouse light house validator node");
        })?;

        Ok(self)
    }

    fn spawn_geth_process(&mut self) -> anyhow::Result<Process> {
        Process::new(
            None,
            self.logs_directory.as_path(),
            self.geth_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--datadir")
                    .arg(&self.execution_layer_data_directory)
                    .arg("--networkid")
                    .arg("420420420")
                    .arg("--ipcpath")
                    .arg(&self.connection_string)
                    .arg("--nodiscover")
                    .arg("--maxpeers")
                    .arg("0")
                    .arg("--nat")
                    .arg("none")
                    .arg("--authrpc.addr")
                    .arg("127.0.0.1")
                    .arg("--authrpc.port")
                    .arg(self.execution_layer_auth_rpc_port.to_string())
                    .arg("--authrpc.vhosts")
                    .arg("localhost")
                    .arg("--authrpc.jwtsecret")
                    .arg(self.shared_secret_file_path.as_path())
                    .arg("--port")
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
                max_wait_duration: self.geth_binary_start_timeout,
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => {
                        if line.contains(Self::ERROR_MARKER) {
                            anyhow::bail!("Failed to start geth {line}");
                        } else {
                            Ok(line.contains(Self::GETH_READY_MARKER))
                        }
                    }
                    None => Ok(false),
                }),
            },
        )
    }

    fn spawn_lighthouse_beacon_node_process(&mut self) -> anyhow::Result<Process> {
        Process::new(
            None,
            self.logs_directory.as_path(),
            // TODO: Consider making this an argument in the CLI.
            self.lighthouse_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--debug-level")
                    .arg("info")
                    .arg("--log-extra-info")
                    .arg("bn")
                    .arg("--testnet-dir")
                    .arg(self.base_directory.as_path())
                    .arg("--datadir")
                    .arg(self.base_directory.as_path())
                    .arg("--execution-endpoint")
                    .arg(format!(
                        "http://127.0.0.1:{}",
                        self.execution_layer_auth_rpc_port
                    ))
                    .arg("--execution-jwt")
                    .arg(self.shared_secret_file_path.as_path())
                    .arg("--disable-discovery")
                    .arg("--boot-nodes")
                    .arg("--listen-address")
                    .arg("127.0.0.1")
                    .arg("--port")
                    .arg(self.beacon_node_listening_port.to_string())
                    .arg("--http")
                    .arg("--http-port")
                    .arg(self.beacon_node_http_port.to_string())
                    .stderr(stderr_file)
                    .stdout(stdout_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: self.lighthouse_binary_start_timeout,
                check_function: Box::new(|_, stderr_line| match stderr_line {
                    Some(line) => {
                        if line.contains(Self::ERROR_MARKER) {
                            anyhow::bail!("Failed to start geth {line}");
                        } else {
                            Ok(line.contains(Self::LIGHTHOUSE_BEACON_NODE_READY_MARKER))
                        }
                    }
                    None => Ok(false),
                }),
            },
        )
    }

    fn spawn_lighthouse_validator_node_process(&mut self) -> anyhow::Result<Process> {
        Process::new(
            None,
            self.logs_directory.as_path(),
            // TODO: Consider making this an argument in the CLI.
            self.lighthouse_binary_path.as_path(),
            |command, stdout_file, stderr_file| {
                command
                    .arg("--debug-level")
                    .arg("info")
                    .arg("--log-extra-info")
                    .arg("vc")
                    .arg("--testnet-dir")
                    .arg(self.base_directory.as_path())
                    .arg("--datadir")
                    .arg(self.base_directory.as_path())
                    .arg("--beacon-nodes")
                    .arg(format!("http://127.0.0.1:{}", self.beacon_node_http_port))
                    .arg("--http")
                    .arg("--http-port")
                    .arg(self.validator_node_http_port.to_string())
                    .arg("--http-token-path")
                    .arg(self.validator_api_token_file_path.as_path())
                    .arg("--suggested-fee-recipient")
                    .arg("0x0000000000000000000000000000000000000000")
                    .arg("--unencrypted-http-transport")
                    .stderr(stderr_file)
                    .stdout(stdout_file);
            },
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration: self.lighthouse_binary_start_timeout,
                check_function: {
                    let path = self.validator_api_token_file_path.clone();
                    Box::new(move |_, _| Ok(path.exists()))
                },
            },
        )
    }

    fn import_validator_keys(&mut self) -> anyhow::Result<()> {
        // TODO: Consider allowing this to be passed in the cli.
        let mut child = Command::new(self.lighthouse_binary_path.as_path())
            .arg("validator_manager")
            .arg("import")
            .arg("--validators-file")
            .arg(self.validators_json_file_path.as_path())
            .arg("--vc-url")
            .arg(format!(
                "http://127.0.0.1:{}",
                self.validator_node_http_port
            ))
            .arg("--vc-token")
            .arg(self.validator_api_token_file_path.as_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn the lighthouse validator manager import command")?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)
            .context("Failed to read the lighthouse validator_manager import stderr")?;

        if !child
            .wait()
            .context("Failed waiting for lighthouse validator_manager import process to finish")?
            .success()
        {
            anyhow::bail!("Failed when calling 'lighthouse validator_manager import': {stderr}");
        }

        Ok(())
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
        drop(self.geth_execution_layer_process.take());

        // Remove the node's database so that subsequent runs do not run on the same database. We
        // ignore the error just in case the directory didn't exist in the first place and therefore
        // there's nothing to be deleted.
        let _ = remove_dir_all(
            self.base_directory
                .join(Self::EXECUTION_LAYER_DATA_DIRECTORY),
        );

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.geth_binary_path)
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

impl Drop for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(geth_node_id = self.id))]
    fn drop(&mut self) {
        self.shutdown().expect("Failed to shutdown")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;

    static DEFAULT_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
        let genesis = include_str!("../../../assets/prod-genesis.json");
        serde_json::from_str(genesis).unwrap()
    });

    fn test_config() -> TestExecutionContext {
        TestExecutionContext::default()
    }

    fn new_node() -> (TestExecutionContext, LighthouseGethNode) {
        let context = test_config();
        let mut node = LighthouseGethNode::new(&context);
        node.init(DEFAULT_GENESIS.clone())
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    fn shared_state() -> &'static (TestExecutionContext, LighthouseGethNode) {
        static STATE: LazyLock<(TestExecutionContext, LighthouseGethNode)> =
            LazyLock::new(new_node);
        &STATE
    }

    fn shared_node() -> &'static LighthouseGethNode {
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
