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
    genesis::Genesis,
    network::{
        EthereumWallet, TransactionBuilder,
    },
    primitives::{
        Address, StorageKey,
        TxHash, U256,
    },
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt,
        trace::geth::{DiffMode, GethDebugTracingOptions},
    },
};
use anyhow::Context as _;
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;

use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;
use zombienet_sdk::{
    LocalFileSystem, NetworkConfigBuilder, NetworkConfigExt,
};

use crate::{
    common::FallbackGasFiller, substrate::ReviveNetwork,
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

    pub fn new(
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
            ..Default::default()
        }
    }

    fn init(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for zombie node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for zombie node")?;

        let _genesis = serde_json::to_value(genesis)?; // TODO: validate this

        let network_config = NetworkConfigBuilder::new()
            .with_relaychain(|r| {
                r.with_chain("rococo-local")
                    .with_default_command("polkadot")
                    //.with_genesis_overrides(zombie_genesis)
                    //.with_chain_spec_path(self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE))
                    .with_node(|node| node.with_name("alice"))
            })
            .with_global_settings(|g| g.with_base_dir(&self.base_directory))
            .with_parachain(|p| {
                p.with_id(Self::PARACHAIN_ID)
                    .evm_based(true)
                    .with_collator(|n| {
                        n.with_name("collator")
                            .with_command("polkadot-parachain")
                            //.with_args(vec!["--rpc-methods=Unsafe".into(), "--rpc-cors=all".into()])
                            .with_ws_port(Self::BASE_RPC_PORT + self.id as u16)
                    })
            })
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build zombienet network config: {e:?}"))?;

        self.network_config = Some(network_config);

        Ok(self)
    }

    async fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        let Some(network_config) = self.network_config.clone() else {
            anyhow::bail!("Node not initialized, call init() first");
        };

        let network = network_config
            .spawn_native()
            .await
            .context("Failed to spawn zombienet network")?;
        network
            .wait_until_is_up(60)
            .await
            .context("Network failed to start within timeout")?;

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
        todo!()
    }

    fn balance_of(
        &self,
        address: Address,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<U256>> + '_>> {
        todo!()
    }

    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<EIP1186AccountProofResponse>> + '_>> {
        todo!()
    }

    fn resolver(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Arc<dyn ResolverApi + '_>>> + '_>> {
        todo!()
    }

    fn evm_version(&self) -> EVMVersion {
        todo!()
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
        context.zombienet_configuration.use_zombienet = true;
        context
    }

    fn new_node() -> (TestExecutionContext, ZombieNode) {
        let context = test_config();
        let mut node = ZombieNode::new(&context);
        let genesis = context.genesis_configuration.genesis().unwrap().clone();
        node.init(genesis).unwrap();
        (context, node)
    }

    #[test]
    fn zombie_node_id_is_unique_and_incremental() {
        let context = test_config();
        let mut ids = Vec::new();
        for _ in 0..5 {
            let node = ZombieNode::new(&context);
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
        tracing::info!("Sending transaction");

        let _ = receipt
            .expect("Failed to send the transfer transaction")
            .get_receipt()
            .await
            .expect("Failed to get the receipt for the transfer");
    }
}
