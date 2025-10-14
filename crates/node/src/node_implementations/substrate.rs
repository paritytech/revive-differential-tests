use std::{
    fs::{create_dir_all, remove_dir_all},
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
use revive_dt_node_interaction::{EthereumNode, MinedBlockInformation};
use subxt::{OnlineClient, SubstrateConfig};
use tokio::sync::OnceCell;
use tracing::{instrument, trace};

use crate::{
    Node,
    constants::{CHAIN_ID, INITIAL_BALANCE},
    helpers::{Process, ProcessReadinessWaitBehavior},
    provider_utils::{
        ConcreteProvider, FallbackGasFiller, construct_concurrency_limited_provider,
        execute_transaction,
    },
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
    provider: OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>,
    consensus: Option<String>,
}

impl SubstrateNode {
    const BASE_DIRECTORY: &str = "substrate";
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
        consensus: Option<String>,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>,
        existing_connection_strings: &[String],
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

        let rpc_url = existing_connection_strings
            .get(id as usize)
            .cloned()
            .unwrap_or_default();

        Self {
            id,
            node_binary: node_path,
            eth_proxy_binary: eth_rpc_path.to_path_buf(),
            export_chainspec_command: export_chainspec_command.to_string(),
            rpc_url,
            base_directory,
            logs_directory,
            substrate_process: None,
            eth_proxy_process: None,
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
            consensus,
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        if !self.rpc_url.is_empty() {
            return Ok(self);
        }

        trace!("Removing the various directories");
        let _ = remove_dir_all(self.base_directory.as_path());
        let _ = clear_directory(&self.base_directory);
        let _ = clear_directory(&self.logs_directory);

        trace!("Creating the various directories");
        create_dir_all(&self.base_directory)
            .context("Failed to create base directory for substrate node")?;
        create_dir_all(&self.logs_directory)
            .context("Failed to create logs directory for substrate node")?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        trace!("Creating the node genesis");
        let chainspec_json = Self::node_genesis(
            &self.node_binary,
            &self.export_chainspec_command,
            &self.wallet,
        )
        .context("Failed to prepare the chainspec command")?;

        trace!("Writing the node genesis");
        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)
                .context("Failed to create substrate template chainspec file")?,
            &chainspec_json,
        )
        .context("Failed to write substrate template chainspec JSON")?;
        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        if !self.rpc_url.is_empty() {
            return Ok(());
        }

        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        let chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        trace!("Spawning the substrate process");
        let substrate_process = Process::new(
            "node",
            self.logs_directory.as_path(),
            self.node_binary.as_path(),
            |command, stdout_file, stderr_file| {
                let cmd = command
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
                    .arg("--pool-limit")
                    .arg(u32::MAX.to_string())
                    .arg("--pool-kbytes")
                    .arg(u32::MAX.to_string())
                    .env("RUST_LOG", Self::SUBSTRATE_LOG_ENV)
                    .stdout(stdout_file)
                    .stderr(stderr_file);
                if let Some(consensus) = self.consensus.as_ref() {
                    cmd.arg("--consensus").arg(consensus.clone());
                }
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

        trace!("Spawning eth-rpc process");
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

    async fn provider(&self) -> anyhow::Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>> {
        self.provider
            .get_or_try_init(|| async move {
                construct_concurrency_limited_provider::<Ethereum, _>(
                    self.rpc_url.as_str(),
                    FallbackGasFiller::new(u64::MAX, 5_000_000_000, 1_000_000_000),
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

    pub fn node_genesis(
        node_path: &Path,
        export_chainspec_command: &str,
        wallet: &EthereumWallet,
    ) -> anyhow::Result<serde_json::Value> {
        let output = Command::new(node_path)
            .arg(export_chainspec_command)
            .arg("--chain")
            .arg("dev")
            .env_remove("RUST_LOG")
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
        let mut chainspec_json = serde_json::from_str::<serde_json::Value>(&content)
            .context("Failed to parse Substrate chain spec JSON")?;

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
        #[subxt::subxt(runtime_metadata_path = "../../assets/revive_metadata.scale")]
        pub mod revive {}

        Box::pin(async move {
            let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
            let substrate_rpc_url = format!("ws://127.0.0.1:{substrate_rpc_port}");
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

pub struct SubstrateNodeResolver {
    id: u32,
    provider: ConcreteProvider<Ethereum, Arc<EthereumWallet>>,
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

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;
    use std::sync::{LazyLock, Mutex};

    use std::fs;

    use super::*;
    use crate::Node;

    fn test_config() -> TestExecutionContext {
        TestExecutionContext::default()
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
            None,
            &context,
            &[],
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
    #[ignore = "Ignored since they take a long time to run"]
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
            None,
            &context,
            &[],
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
    fn version_works() {
        let node = shared_node();

        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected Substrate-node version string, got: {version}"
        );
    }

    #[test]
    #[ignore = "Ignored since they take a long time to run"]
    fn eth_rpc_version_works() {
        let node = shared_node();

        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }

    #[tokio::test]
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
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
    #[ignore = "Ignored since they take a long time to run"]
    async fn can_get_block_number_from_node() {
        // Arrange
        let node = shared_node();

        // Act
        let block_number = node.resolver().await.unwrap().last_block_number().await;

        // Assert
        let _ = block_number.expect("Failed to get the block number");
    }
}
