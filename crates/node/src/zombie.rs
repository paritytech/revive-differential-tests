use core::net;
use std::{
    fs::{File, create_dir_all},
    path::PathBuf,
    pin::Pin,
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use alloy::{
    consensus::{BlockHeader, TxEnvelope},
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{
        self, Ethereum, EthereumWallet, Network, NetworkWallet, TransactionBuilder,
        TransactionBuilderError, UnbuiltTransactionError,
    },
    primitives::{
        Address, B64, B256, BlockHash, BlockNumber, BlockTimestamp, Bloom, Bytes, StorageKey,
        TxHash, U256,
    },
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt,
        eth::{Block, Header, Transaction},
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use anyhow::Context as _;
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;

use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;
use tracing::info;
use zombienet_sdk::{LocalFileSystem, NetworkConfigBuilder, NetworkConfigExt, subxt};

use crate::{
    common::FallbackGasFiller,
    constants::INITIAL_BALANCE,
    substrate::{ReviveNetwork, SubstrateNodeResolver},
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Default)]
pub struct ZombieNode {
    id: u32,
    node_binary: PathBuf,
    export_chainspec_command: String,
    connection_string: String,
    base_directory: PathBuf,
    logs_directory: PathBuf,
    process_proxy: Option<Child>,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    chain_id_filler: ChainIdFiller,
    logs_file_to_flush: Vec<File>,
    network_config: Option<zombienet_sdk::NetworkConfig>,
    network: Option<zombienet_sdk::Network<LocalFileSystem>>,
}

impl ZombieNode {
    const BASE_DIRECTORY: &str = "zombienet";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";

    const BASE_RPC_PORT: u16 = 9944;
    const PARACHAIN_ID: u32 = 100;

    const ZOMBIENET_STDOUT_LOG_FILE_NAME: &str = "node_stdout.log";
    const ZOMBIENET_STDERR_LOG_FILE_NAME: &str = "node_stderr.log";

    pub const SUBSTRATE_EXPORT_CHAINSPEC_COMMAND: &str = "export-chain-spec";

    pub fn new(
        node_path: PathBuf,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>,
    ) -> Self {
        let working_directory_path = AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path
            .as_path()
            .join(Self::BASE_DIRECTORY)
            .join(id.to_string());
        let logs_directory = base_directory.join(Self::LOGS_DIRECTORY);
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        Self {
            id,
            base_directory,
            logs_directory,
            wallet,
            logs_file_to_flush: Vec::with_capacity(2),
            node_binary: node_path,
            ..Default::default()
        }
    }

    fn init(&mut self, mut genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for zombie node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for zombie node")?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);
        self.prepare_chainspec(template_chainspec_path.clone(), genesis)?;

        let node_binary = self.node_binary.to_str().unwrap_or_default();

        let network_config = NetworkConfigBuilder::new()
            .with_relaychain(|r| {
                r.with_chain("rococo-local")
                    .with_default_command("polkadot")
                    .with_node(|node| node.with_name("alice"))
            })
            .with_global_settings(|g| g.with_base_dir(&self.base_directory))
            .with_parachain(|p| {
                p.with_id(Self::PARACHAIN_ID)
                    .with_chain_spec_path(template_chainspec_path.to_str().unwrap())
                    .with_collator(|n| {
                        n.with_name("collator")
                            .with_command(node_binary)
                            .with_args(vec!["--dev".into()])
                            .with_ws_port(Self::BASE_RPC_PORT + self.id as u16)
                    })
            })
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build zombienet network config: {e:?}"))?;

        self.network_config = Some(network_config);

        Ok(self)
    }

    fn prepare_chainspec(
        &mut self,
        template_chainspec_path: PathBuf,
        mut genesis: Genesis,
    ) -> anyhow::Result<()> {
        let output = std::process::Command::new(&self.node_binary)
            .arg(Self::SUBSTRATE_EXPORT_CHAINSPEC_COMMAND)
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

        let writer = std::fs::File::create(&template_chainspec_path)
            .context("Failed to create substrate template chainspec file")?;

        serde_json::to_writer_pretty(writer, &chainspec_json)
            .context("Failed to write substrate template chainspec JSON")?;

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

    async fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let Some(network_config) = self.network_config.clone() else {
            anyhow::bail!("Node not initialized, call init() first");
        };

        let network = network_config
            .spawn_native()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to spawn zombienet network: {e:?}"))?;

        tracing::info!("Zombienet network is up");

        let ws_uri = network
            .parachain(Self::PARACHAIN_ID)
            .context("Failed to get parachain from zombienet network")?
            .collators()
            .first()
            .context("No collators found in zombienet parachain")?
            .ws_uri();
        let ws_uri = ws_uri.replace("ws", "http");
        self.connection_string = ws_uri.to_string();
        self.network = Some(network);

        Ok(self)
    }

    async fn provider(
        &self,
    ) -> anyhow::Result<
        FillProvider<impl TxFiller<ReviveNetwork>, impl Provider<ReviveNetwork>, ReviveNetwork>,
    > {
        let Some(_network) = &self.network else {
            anyhow::bail!("Node not initialized, call spawn() first");
        };

        Ok(ProviderBuilder::new()
            .disable_recommended_fillers()
            .network::<ReviveNetwork>()
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
            .context("Failed to connect to parachain Ethereum RPC")?)
    }
}

impl EthereumNode for ZombieNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        &self.connection_string
    }

    fn execute_transaction(
        &self,
        transaction: alloy::rpc::types::TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TransactionReceipt>> + '_>> {
        Box::pin(async move {
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
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::rpc::types::trace::geth::GethTrace>> + '_>>
    {
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
            Ok(Arc::new(SubstrateNodeResolver { id, provider }) as Arc<dyn ResolverApi>)
        })
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;
    use std::{sync,fs};
    
    use super::*;
    use crate::Node;

    fn test_config() -> TestExecutionContext {
        let mut context = TestExecutionContext::default();
        context.zombienet_configuration.use_zombienet = true;
        context
    }

    fn new_node() -> (TestExecutionContext, ZombieNode) {
        let context = test_config();
        let mut node = ZombieNode::new(context.zombienet_configuration.path.clone(), &context);
        let genesis = context.genesis_configuration.genesis().unwrap().clone();
        node.init(genesis).unwrap();
        (context, node)
    }

    #[test]
    fn zombie_node_id_is_unique_and_incremental() {
        let context = test_config();
        let mut ids = Vec::new();
        for _ in 0..5 {
            let (_, node) = new_node();
            ids.push(node.id);
        }
        // Check uniqueness
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "Node ids should be unique");
        // Check strictly increasing
        for w in ids.windows(2) {
            assert!(w[1] > w[0], "Node ids should be strictly increasing");
        }
    }

    #[test]
    fn zombie_node_spawn() {
        let (context, mut node) = new_node();
        let genesis = context.genesis_configuration.genesis().unwrap().clone();
        let network = node.init(genesis).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async { network.spawn_process().await });

        assert!(result.is_ok(), "Zombienet should spawn successfully");
    }

    #[tokio::test]
    async fn test_transfer_transaction_should_return_receipt() {
        use tracing_subscriber::filter::LevelFilter;
        use tracing_subscriber::{EnvFilter, FmtSubscriber};
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::builder()
                    .with_default_directive(LevelFilter::DEBUG.into())
                    .from_env_lossy(),
            )
            .init();

        let (context, mut node) = new_node();

        let node = node.spawn_process().await.unwrap();

        let provider = node.provider().await.expect("Failed to create provider");
        let account_address = context
            .wallet_configuration
            .wallet()
            .default_signer()
            .address();
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
}
