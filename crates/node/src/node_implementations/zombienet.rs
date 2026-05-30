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

#![allow(dead_code)]

use alloy::{eips::Encodable2718, primitives::TxHash};
use futures::FutureExt;

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A Zombienet network where collator is `polkadot-parachain` node with `eth-rpc` [`ZombieNode`]
/// abstracts away the details of managing the zombienet network and provides an interface to
/// interact with the parachain's Ethereum RPC.
#[derive(Debug)]
pub struct ZombienetNode {
    /* Node Identifier */
    id: u32,

    /* Directory Paths */
    directories: NodeDirectories,

    /* Binary Paths & Timeouts */
    eth_rpc_binary: PathBuf,
    polkadot_parachain_path: PathBuf,

    /* Spawned Processes */
    eth_rpc_process: Option<EthRpcProcess>,
    zombienet_process: Option<ZombienetProcess>,

    /* Zombienet Network */
    config_path: Option<PathBuf>,
    network_config: Option<zombienet_sdk::NetworkConfig>,

    /* Provider Related Fields */
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,

    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,

    use_fallback_gas_filler: bool,

    eth_rpc_logging_level: String,

    /// All transaction submissions in zombienet go through subxt rather than alloy so concurrency
    /// limiting of submissions needs to be part of the node itself.
    submission_semaphore: Arc<Semaphore>,

    /// The maximum duration to wait for the parachain to start producing blocks after the
    /// zombienet network is spawned.
    block_production_timeout: Duration,
}

impl ZombienetNode {
    pub fn new(
        polkadot_parachain_path: PathBuf,
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasZombienetConfiguration,
        use_fallback_gas_filler: bool,
    ) -> Self {
        let eth_rpc_binary = context.as_eth_rpc_configuration().path.to_owned();
        let working_directory_path = context.as_working_directory_configuration();
        let zombienet_configuration = context.as_zombienet_configuration();
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let directories = NodeDirectories::new(
            working_directory_path.working_directory.as_path(),
            "zombienet",
            id,
        )
        .expect("TODO(constructors): Remove this when we have failing constructors");
        let wallet = context.as_wallet_configuration().wallet();

        Self {
            id,
            directories,
            wallet,
            polkadot_parachain_path,
            eth_rpc_binary,
            nonce_manager: CachedNonceManager::default(),
            config_path: zombienet_configuration.config_path.clone(),
            network_config: Default::default(),
            eth_rpc_process: Default::default(),
            zombienet_process: Default::default(),
            provider: Default::default(),
            substrate_provider: Default::default(),
            use_fallback_gas_filler,
            eth_rpc_logging_level: context.as_eth_rpc_configuration().logging_level.clone(),
            block_production_timeout: zombienet_configuration.block_production_timeout_ms,
            submission_semaphore: Arc::new(Semaphore::new(1000)),
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        let config_path = self.config_path.as_ref().context(
            "A zombienet config file path is required. Provide one via --zombienet.config-path",
        )?;

        let toml_content =
            std::fs::read_to_string(config_path).context("Failed to read zombienet config file")?;
        let mut toml_value: toml::Value =
            toml::from_str(&toml_content).context("Failed to parse zombienet TOML")?;

        self.inject_prefunded_chainspec(&mut toml_value)?;
        self.make_node_names_unique(&mut toml_value);

        let modified_toml_path = self.directories.base_directory().join("zombienet.toml");
        std::fs::write(
            &modified_toml_path,
            toml::to_string(&toml_value).context("Failed to serialize modified TOML")?,
        )
        .context("Failed to write modified zombienet config")?;

        let network_config = NetworkConfig::load_from_toml(
            modified_toml_path
                .to_str()
                .context("Invalid modified TOML path")?,
        )
        .context("Failed to load zombienet config")?;

        self.network_config = Some(network_config);

        Ok(self)
    }

    /// Appends the node's numeric ID to every node name in the TOML config so that each
    /// Zombienet instance gets unique libp2p peer identities. Without this, two instances
    /// spawned from the same TOML would have identical node keys (derived from
    /// `SHA256(node_name)`) and discover/interfere with each other.
    fn make_node_names_unique(&self, toml_value: &mut toml::Value) {
        let suffix = format!("-{}", self.id);

        let append_suffix = |nodes: &mut Vec<toml::Value>| {
            for node in nodes.iter_mut() {
                if let Some(name) = node
                    .get_mut("name")
                    .and_then(|v| v.as_str().map(String::from))
                {
                    node.as_table_mut().unwrap().insert(
                        "name".into(),
                        toml::Value::String(format!("{name}{suffix}")),
                    );
                }
            }
        };

        if let Some(nodes) = toml_value
            .get_mut("relaychain")
            .and_then(|r| r.get_mut("nodes"))
            .and_then(|n| n.as_array_mut())
        {
            append_suffix(nodes);
        }

        if let Some(parachains) = toml_value
            .get_mut("parachains")
            .and_then(|p| p.as_array_mut())
        {
            for parachain in parachains.iter_mut() {
                if let Some(collators) = parachain
                    .get_mut("collators")
                    .and_then(|c| c.as_array_mut())
                {
                    append_suffix(collators);
                }
            }
        }
    }

    /// Runs the parachain's `chain_spec_command`, appends wallet balances to the generated
    /// chainspec, writes the result to a file, and replaces `chain_spec_command` with
    /// `chain_spec_path` in the TOML config.
    fn inject_prefunded_chainspec(&self, toml_value: &mut toml::Value) -> anyhow::Result<()> {
        let parachain = toml_value
            .get_mut("parachains")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.first_mut())
            .context("No [[parachains]] found in zombienet config")?;

        let chain_name = parachain
            .get("chain")
            .and_then(|v| v.as_str())
            .unwrap_or("asset-hub-westend-local");

        let command_template = parachain
            .get("chain_spec_command")
            .and_then(|v| v.as_str())
            .context("No chain_spec_command found in the parachain config")?;
        let command = command_template.replace("{{chainName}}", chain_name);

        // Run the chain_spec_command to get the base chainspec.
        let output = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env_remove("RUST_LOG")
            .run_and_get_output()
            .context("Failed to run chain_spec_command")?;

        let mut chainspec = serde_json::from_str::<serde_json::Value>(&output.stdout)
            .context("Failed to parse chainspec JSON")?;

        // Append wallet balances to the existing balances array.
        inject_wallet_balances(&mut chainspec, &self.wallet)?;

        // Write the pre-funded chainspec to a file.
        let chainspec_path = self.directories.base_directory().join("chainspec.json");
        std::fs::write(
            &chainspec_path,
            serde_json::to_string_pretty(&chainspec).context("Failed to serialize chainspec")?,
        )
        .context("Failed to write chainspec")?;

        tracing::info!(
            path = %chainspec_path.display(),
            "Generated pre-funded chainspec"
        );

        // Swap chain_spec_command → chain_spec_path in the TOML.
        let table = parachain
            .as_table_mut()
            .context("Parachain config is not a table")?;
        table.remove("chain_spec_command");
        table.insert(
            "chain_spec_path".to_string(),
            toml::Value::String(chainspec_path.to_string_lossy().into_owned()),
        );

        Ok(())
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let network_config = self
            .network_config
            .clone()
            .context("Node not initialized, call init() first")?;

        self.zombienet_process = ZombienetProcess::new(network_config)
            .inspect_err(|err| error!(error = ?err, "Failed to spawn zombienet"))?
            .into();

        self.eth_rpc_process = EthRpcProcess::new(
            self.eth_rpc_binary.as_path(),
            self.directories.logs_directory(),
            self.zombienet_process
                .as_ref()
                .expect("qed; we initialized it")
                .url(),
            self.eth_rpc_logging_level.as_str(),
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?
        .into();

        Ok(())
    }

    fn provider(
        &self,
    ) -> FrameworkFuture<anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_string();
        let nonce_manager = self.nonce_manager.clone();
        let wallet = self.wallet.clone();
        let use_fallback_gas_filler = self.use_fallback_gas_filler;

        Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    construct_concurrency_limited_provider(
                        &connection_string,
                        FallbackGasFiller::default()
                            .with_fallback_mechanism(use_fallback_gas_filler),
                        ChainIdFiller::default(),
                        NonceFiller::new(nonce_manager),
                        wallet,
                    )
                    .await
                    .context("Failed to construct the provider")
                })
                .await
                .cloned()
        })
    }

    pub fn node_genesis(
        node_path: &Path,
        wallet: &EthereumWallet,
    ) -> anyhow::Result<serde_json::Value> {
        let output = Command::new(node_path)
            .arg("build-spec")
            .arg("--chain")
            .arg("asset-hub-westend-local")
            .env_remove("RUST_LOG")
            .run_and_get_output()
            .context("Failed to export the chainspec of the chain")?;

        let mut chainspec_json = serde_json::from_str::<serde_json::Value>(&output.stdout)
            .context("Failed to parse Substrate chain spec JSON")?;

        inject_wallet_balances(&mut chainspec_json, wallet)?;

        Ok(chainspec_json)
    }
}

impl NodeApi for ZombienetNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.eth_rpc_process
            .as_ref()
            .expect("must be initialized")
            .url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn submit_transaction(
        &self,
        mut transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<TxHash>> {
        transaction.set_gas_price(u128::MAX);

        let provider = self.provider();
        let substrate_provider = NodeApi::substrate_provider(self);
        let semaphore = self.submission_semaphore.clone();

        Box::pin(async move {
            let provider = provider.await.context("Failed to get the provider")?;
            let substrate_provider = substrate_provider
                .expect("qed; this is a substrate node")
                .await
                .context("Failed to get the provider")?;

            let signed_transaction = provider
                .fill(transaction)
                .await
                .context("Failed to fill transaction")?
                .try_into_envelope()
                .context("Failed to construct envelope from filled transaction")?;
            let tx_hash = signed_transaction.tx_hash();
            let payload = signed_transaction.encoded_2718();

            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec());
            let _guard = semaphore
                .acquire_owned()
                .await
                .context("Failed to acquire permit")?;
            substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create an unsigned transaction")?
                .submit()
                .await
                .context("Failed to submit the transaction through subxt")?;

            Ok(*tx_hash)
        })
    }

    fn provider(&self) -> revive_dt_common::futures::FrameworkFuture<anyhow::Result<DynProvider>> {
        Box::pin(
            self.provider()
                .map(|maybe_provider| maybe_provider.map(|provider| provider.erased())),
        )
    }

    fn substrate_provider(
        &self,
    ) -> Option<
        revive_dt_common::futures::FrameworkFuture<anyhow::Result<OnlineClient<PolkadotConfig>>>,
    > {
        let provider = self.substrate_provider.clone();
        let connection_string = self
            .zombienet_process
            .as_ref()
            .expect("must be initialized")
            .url()
            .to_string();
        Some(Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    OnlineClient::from_url(connection_string)
                        .await
                        .context("Failed to create a new online client")
                })
                .await
                .cloned()
        }))
    }

    fn substrate_rpc_client(
        &self,
    ) -> Option<FrameworkFuture<Result<subxt::backend::rpc::RpcClient>>> {
        let connection_string = self
            .zombienet_process
            .as_ref()
            .expect("must be initialized")
            .url()
            .to_string();
        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(connection_string)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for ZombienetNode {
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }
}
