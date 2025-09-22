use std::{
    fs::{File, OpenOptions, create_dir_all, remove_dir_all},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    pin::Pin,
    process::{Child, Command, Stdio},
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
use serde::{Deserialize, Serialize, de};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;

use revive_dt_config::*;
use revive_dt_node_interaction::EthereumNode;
use tracing::instrument;

use crate::{Node, common::FallbackGasFiller, constants::INITIAL_BALANCE};

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
}

impl ZombieNode {
    const BASE_DIRECTORY: &str = "zombienet";
    const DATA_DIRECTORY: &str = "data";
    const LOGS_DIRECTORY: &str = "logs";

    const ZOMBIENET_STDOUT_LOG_FILE_NAME: &str = "node_stdout.log";
    const ZOMBIENET_STDERR_LOG_FILE_NAME: &str = "node_stderr.log";

    pub fn new(
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<EthRpcConfiguration>
        + AsRef<WalletConfiguration>,
    ) -> Self {
        let working_directory_path = AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = working_directory_path.as_path().join(Self::BASE_DIRECTORY);
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

    fn init(&mut self, mut genesis: Genesis) -> anyhow::Result<&mut Self> {
        todo!()
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        todo!()
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
        todo!()
    }

    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloy::rpc::types::trace::geth::GethTrace>> + '_>>
    {
        todo!()
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
