use std::{
    fs::{create_dir_all, remove_dir_all},
    path::PathBuf,
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use alloy::{
    eips::BlockNumberOrTag,
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, EthereumWallet, NetworkWallet},
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, StorageKey, TxHash, U256},
    providers::{
        Provider, ProviderBuilder,
        ext::DebugApi,
        fillers::{CachedNonceManager, ChainIdFiller, FillProvider, NonceFiller, TxFiller},
    },
    rpc::types::{
        EIP1186AccountProofResponse, TransactionReceipt,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use anyhow::Context as _;
use revive_common::EVMVersion;
use revive_dt_common::fs::clear_directory;
use revive_dt_format::traits::ResolverApi;

use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;
use tracing::instrument;
use zombienet_sdk::{LocalFileSystem, NetworkConfigBuilder, NetworkConfigExt};

use crate::{
    Node, common::FallbackGasFiller, constants::INITIAL_BALANCE, substrate::ReviveNetwork,
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Default)]
pub struct ZombieNode {
    id: u32,
    node_binary: PathBuf,
    connection_string: String,
    base_directory: PathBuf,
    logs_directory: PathBuf,
    eth_proxy_binary: PathBuf,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    chain_id_filler: ChainIdFiller,
    network_config: Option<zombienet_sdk::NetworkConfig>,
    network: Option<zombienet_sdk::Network<LocalFileSystem>>,
    eth_rpc_process: Option<std::process::Child>,
}

impl ZombieNode {
    const BASE_DIRECTORY: &str = "zombienet";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const BASE_RPC_PORT: u16 = 9944;
    const PARACHAIN_ID: u32 = 100;

    const EXPORT_CHAINSPEC_COMMAND: &str = "build-spec";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";

    pub fn new(
        node_path: PathBuf,
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
            node_binary: node_path,
            eth_proxy_binary,
            nonce_manager: CachedNonceManager::default(),
            chain_id_filler: ChainIdFiller::default(),
            network_config: None,
            network: None,
            eth_rpc_process: None,
            connection_string: String::new(),
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
                    .with_chain("asset-hub-westend-local")
                    .with_collator(|n| {
                        n.with_name("Collator")
                            .with_command(node_binary)
                            .with_rpc_port(Self::BASE_RPC_PORT + self.id as u16)
                    })
            })
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build zombienet network config: {e:?}"))?;

        self.network_config = Some(network_config);

        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let Some(network_config) = self.network_config.clone() else {
            anyhow::bail!("Node not initialized, call init() first");
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let network = rt.block_on(async {
            network_config
                .spawn_native()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to spawn zombienet network: {e:?}"))
        })?;

        tracing::info!("Zombienet network is up");

        let log_file =
            std::fs::File::create("eth-rpc.log").context("Failed to create eth-rpc log file")?;
        let child = Command::new("eth-rpc")
            .arg("--rpc-cors")
            .arg("all")
            .arg("--rpc-methods")
            .arg("Unsafe")
            .arg("--rpc-max-connections")
            .arg(u32::MAX.to_string())
            .stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file))
            .spawn()
            .context("Failed to spawn eth-rpc process")?;

        std::thread::sleep(std::time::Duration::from_secs(5));
        tracing::info!("eth-rpc is up");

        self.connection_string = "http://localhost:8545".to_string();
        self.eth_rpc_process = Some(child);
        self.network = Some(network);

        Ok(())
    }

    fn prepare_chainspec(
        &mut self,
        template_chainspec_path: PathBuf,
        mut genesis: Genesis,
    ) -> anyhow::Result<()> {
        let output = std::process::Command::new(&self.node_binary)
            .arg(Self::EXPORT_CHAINSPEC_COMMAND)
            .arg("--chain")
            .arg("asset-hub-westend-local")
            .output()
            .context("Failed to export the chain-spec")?;

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
    ) -> anyhow::Result<
        FillProvider<impl TxFiller<ReviveNetwork>, impl Provider<ReviveNetwork>, ReviveNetwork>,
    > {
        let Some(_network) = &self.network else {
            anyhow::bail!("Node not initialized, call spawn() first");
        };

        ProviderBuilder::new()
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
            .context("Failed to connect to parachain Ethereum RPC")
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
                .context("Failed to submit transaction to proxy")?
                .get_receipt()
                .await
                .context("Failed to fetch transaction receipt from proxy")?;
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
}

pub struct ZombieNodeResolver<F: TxFiller<ReviveNetwork>, P: Provider<ReviveNetwork>> {
    pub(crate) id: u32,
    pub(crate) provider: FillProvider<F, P, ReviveNetwork>,
}

impl<F: TxFiller<ReviveNetwork>, P: Provider<ReviveNetwork>> ResolverApi
    for ZombieNodeResolver<F, P>
{
    #[instrument(level = "info", skip_all, fields(zombie_node_id = self.id))]
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

impl Node for ZombieNode {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        // TODO: destroy the zombienet network properly

        let base_directory = self.base_directory.clone();
        let data_directory = PathBuf::from(Self::DATA_DIRECTORY);

        // Take the process handle
        let eth_rpc_process = self.eth_rpc_process.take();
        // Kill the eth_rpc process
        let _ = eth_rpc_process.map(|mut child| child.kill());

        // Remove the database directory
        let _ = remove_dir_all(base_directory.join(data_directory));

        // Return immediately
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
            .context("Failed execute --version")?
            .wait_with_output()
            .context("Failed to wait --version")?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for ZombieNode {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;

    use super::*;
    use crate::Node;

    fn test_config() -> TestExecutionContext {
        let mut context = TestExecutionContext::default();
        context.zombienet_configuration.use_zombienet = true;
        context
    }

    async fn new_node() -> (TestExecutionContext, ZombieNode) {
        let context = test_config();
        let mut node = ZombieNode::new(context.zombienet_configuration.path.clone(), &context);
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

    #[tokio::test]
    async fn eth_rpc_version_works() {
        let (_ctx, node) = new_node().await;

        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }

    #[tokio::test]
    async fn zombie_node_id_is_unique_and_incremental() {
        let mut ids = Vec::new();
        for _ in 0..5 {
            let (_, node) = new_node().await;
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
    #[tokio::test]
    async fn version_works() {
        let (_ctx, node) = new_node().await;

        let version = node.version().unwrap();

        assert!(
            version.starts_with("polkadot-parachain"),
            "Expected Polkadot-parachain version string, got: {version}"
        );
    }

    #[tokio::test]
    async fn get_chain_id_from_node_should_succeed() {
        let (_context, node) = new_node().await;

        let chain_id = node
            .resolver()
            .await
            .expect("Failed to create resolver")
            .chain_id()
            .await
            .expect("Failed to get chain id");

        assert_eq!(chain_id, 420_420_421, "Chain id should be 420_420_421");
    }

    #[tokio::test]
    async fn can_get_gas_limit_from_node() {
        // Arrange
        let (_context, node) = new_node().await;

        // Act
        let gas_limit = node
            .resolver()
            .await
            .unwrap()
            .block_gas_limit(BlockNumberOrTag::Latest)
            .await;
        println!("Gas limit: {:?}", gas_limit);
        // Assert
        let _ = gas_limit.expect("Failed to get the gas limit");
    }

    #[tokio::test]
    async fn can_get_coinbase_from_node() {
        // Arrange
        let (_context, node) = new_node().await;

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
        let (_context, node) = new_node().await;

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
        let (_context, node) = new_node().await;

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
        let (_context, node) = new_node().await;

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
        let (_context, node) = new_node().await;

        // Act
        let block_number = node.resolver().await.unwrap().last_block_number().await;

        // Assert
        let _ = block_number.expect("Failed to get the block number");
    }

    #[tokio::test]
    async fn test_transfer_transaction_should_return_receipt() {
        let (context, node) = new_node().await;

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
