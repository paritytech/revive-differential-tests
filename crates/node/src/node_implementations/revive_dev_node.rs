#![allow(dead_code)]

use crate::internal_prelude::*;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// A node implementation for Substrate based chains. Currently, this supports either substrate
/// or the revive-dev-node which is done by changing the path and some of the other arguments passed
/// to the command.
#[derive(Debug)]

pub struct ReviveDevNode {
    id: u32,
    node_binary: PathBuf,
    eth_rpc_binary: PathBuf,
    directories: NodeDirectories,
    revive_dev_node_process: Option<ReviveDevNodeProcess>,
    eth_rpc_process: Option<EthRpcProcess>,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,
    consensus: Option<String>,
    use_fallback_gas_filler: bool,
    node_logging_level: String,
    eth_rpc_logging_level: String,
}

impl ReviveDevNode {
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";

    /// Returns the WebSocket URL for the substrate RPC endpoint of this node.
    fn substrate_ws_url(&self) -> String {
        self.revive_dev_node_process
            .as_ref()
            .expect("must be initialized")
            .url()
            .to_string()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_path: PathBuf,
        consensus: Option<String>,
        context: impl HasWorkingDirectoryConfiguration + HasEthRpcConfiguration + HasWalletConfiguration,
        use_fallback_gas_filler: bool,
        node_logging_level: String,
        eth_rpc_logging_level: String,
    ) -> Self {
        let working_directory_path = context
            .as_working_directory_configuration()
            .working_directory
            .as_path();
        let eth_rpc_path = context.as_eth_rpc_configuration().path.as_path();
        let wallet = context.as_wallet_configuration().wallet();

        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let directories = NodeDirectories::new(working_directory_path, "revive-dev-node", id)
            .expect("TODO(constructors): Remove this when we have failing constructors");

        Self {
            id,
            node_binary: node_path,
            eth_rpc_binary: eth_rpc_path.to_path_buf(),
            directories,
            revive_dev_node_process: Default::default(),
            eth_rpc_process: Default::default(),
            wallet: wallet.clone(),
            nonce_manager: Default::default(),
            provider: Default::default(),
            substrate_provider: Default::default(),
            consensus,
            use_fallback_gas_filler,
            node_logging_level,
            eth_rpc_logging_level,
        }
    }

    fn init(&mut self, _: Genesis) -> anyhow::Result<&mut Self> {
        static CHAINSPEC_MUTEX: StdMutex<Option<Value>> = StdMutex::new(None);

        trace!("Creating the node genesis");
        let chainspec_json = {
            let mut chainspec_mutex = CHAINSPEC_MUTEX.lock().expect("Poisoned");
            match chainspec_mutex.as_ref() {
                Some(chainspec_json) => chainspec_json.clone(),
                None => {
                    let chainspec_json = Self::node_genesis(&self.node_binary, &self.wallet)
                        .context("Failed to prepare the chainspec command")?;
                    *chainspec_mutex = Some(chainspec_json.clone());
                    chainspec_json
                }
            }
        };

        let template_chainspec_path = self
            .directories
            .base_directory()
            .join(Self::CHAIN_SPEC_JSON_FILE);

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
        self.revive_dev_node_process = ReviveDevNodeProcess::new(
            self.node_binary.as_path(),
            self.directories
                .base_directory()
                .join(Self::CHAIN_SPEC_JSON_FILE),
            self.consensus.as_deref().unwrap_or("instant-seal"),
            self.directories.data_directory(),
            self.directories.logs_directory(),
            self.node_logging_level.as_str(),
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn revive-dev-node"))?
        .into();

        self.eth_rpc_process = EthRpcProcess::new(
            self.eth_rpc_binary.as_path(),
            self.directories.logs_directory(),
            self.revive_dev_node_process
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
        let gas_filler =
            FallbackGasFiller::default().with_fallback_mechanism(self.use_fallback_gas_filler);
        let nonce_filler = NonceFiller::new(self.nonce_manager.clone());
        let wallet = self.wallet.clone();

        Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    construct_concurrency_limited_provider::<Ethereum, _>(
                        &connection_string,
                        gas_filler,
                        ChainIdFiller::default(),
                        nonce_filler,
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
        trace!("Exporting the chainspec");
        let output = Command::new(node_path)
            .arg("build-spec")
            .arg("--chain")
            .arg("dev")
            .env_remove("RUST_LOG")
            .run_and_get_output()
            .context("Failed to build the chainspec")?;

        let mut chainspec_json = serde_json::from_str::<serde_json::Value>(&output.stdout)
            .context("Failed to parse Substrate chain spec JSON")?;

        trace!("Adding addresses to chainspec");
        inject_wallet_balances(&mut chainspec_json, wallet)?;

        Ok(chainspec_json)
    }
}

impl NodeApi for ReviveDevNode {
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

    /// Submits a transaction and watches for it and if the transaction is dropped from the mempool
    /// then it resubmits it again. Times out after 5 minutes if the transaction is not finalized in
    /// that time frame.
    fn submit_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<alloy::primitives::TxHash>> {
        let provider = Self::provider(self);
        let substrate_provider = NodeApi::substrate_provider(self);

        let span = debug_span!(
            "Revive dev node special transaction submission flow",
            ethereum_tx_hash = tracing::field::Empty,
            substrate_tx_hash = tracing::field::Empty,
        );

        let task = async move {
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
            let ethereum_tx_hash = signed_transaction.tx_hash();
            let payload = signed_transaction.encoded_2718();

            Span::current().record(
                "ethereum_tx_hash",
                tracing::field::display(ethereum_tx_hash),
            );

            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec())
                .unvalidated();
            let mut tx_progress = substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create an unsigned transaction")?
                .submit_and_watch()
                .await
                .context("Failed to submit the transaction through subxt")?;

            Span::current().record(
                "substrate_tx_hash",
                tracing::field::debug(tx_progress.extrinsic_hash()),
            );

            debug!("Submitted a substrate transaction");

            timeout(Duration::from_secs(5 * 60), async move {
                loop {
                    match tx_progress.next().await {
                        Some(progress) => {
                            let progress =
                                progress.context("Failed to get the transaction progress update")?;

                            // Logging the progress should be separate since I can't use the debug
                            // formatting since it's way way way too verbose for anything that is
                            // sane for logging.
                            match &progress {
                                TxStatus::Validated => debug!(
                                    update = "Validated",
                                    "Received a transaction progress update"
                                ),
                                TxStatus::Broadcasted => {
                                    debug!(
                                        update = "Broadcasted",
                                        "Received a transaction progress update"
                                    )
                                },
                                TxStatus::NoLongerInBestBlock => {
                                    debug!(
                                        update = "NoLongerInBestBlock",
                                        "Received a transaction progress update"
                                    )
                                },
                                TxStatus::InBestBlock(tx_in_block) => {
                                    debug!(
                                        update = "InBestBlock",
                                        block_hash = %tx_in_block.block_hash(),
                                        "Received a transaction progress update"
                                    )
                                },
                                TxStatus::InFinalizedBlock(tx_in_block) => {
                                    debug!(
                                        update = "InFinalizedBlock",
                                        block_hash = %tx_in_block.block_hash(),
                                        "Received a transaction progress update"
                                    )
                                },
                                TxStatus::Error { message } => {
                                    debug!(update = "Error", message, "Received a transaction progress update")
                                },
                                TxStatus::Invalid { message } => {
                                    debug!(update = "Invalid", message, "Received a transaction progress update")
                                },
                                TxStatus::Dropped { message } => {
                                    debug!(update = "Dropped", message, "Received a transaction progress update")
                                },
                            }

                            match progress {
                                TxStatus::InFinalizedBlock(..) => {
                                    debug!("Transaction has been finalized, stopping watching");
                                    break Ok(());
                                }
                                TxStatus::Invalid { message } => {
                                    error!(message, "Transaction is invalid!");
                                    bail!("Encountered an invalid transaction. Message = {message}")
                                }
                                TxStatus::Error {  message } | TxStatus::Dropped {  message } => {
                                    warn!(
                                        message,
                                        "Transaction was either dropped or encountered an error - resubmitting"
                                    );
                                    tx_progress = substrate_provider
                                        .tx()
                                        .create_unsigned(&call)
                                        .context("Failed to create an unsigned transaction")?
                                        .submit_and_watch()
                                        .await
                                        .context("Failed to resubmit the transaction through subxt")?
                                },
                                TxStatus::Validated
                                | TxStatus::Broadcasted
                                | TxStatus::NoLongerInBestBlock
                                | TxStatus::InBestBlock(..) => {}
                            }
                        }
                        None => {
                            error!(
                                "Stopped getting transaction progress updates before it was finalized"
                            );
                            tx_progress = substrate_provider
                                .tx()
                                .create_unsigned(&call)
                                .context("Failed to create an unsigned transaction")?
                                .submit_and_watch()
                                .await
                                .context("Failed to resubmit the transaction through subxt")?
                        }
                    }
                }
            })
            .await
            .context("Submission failed due to a timeout after watching for 5 minutes")??;

            Ok(*ethereum_tx_hash)
        }
        .instrument(span);
        Box::pin(task)
    }

    fn provider(&self) -> revive_dt_common::futures::FrameworkFuture<anyhow::Result<DynProvider>> {
        Self::provider(self)
            .map(|provider| provider.map(|provider| provider.erased()))
            .boxed()
    }

    fn substrate_provider(
        &self,
    ) -> Option<
        revive_dt_common::futures::FrameworkFuture<anyhow::Result<OnlineClient<PolkadotConfig>>>,
    > {
        let provider = self.substrate_provider.clone();
        let connection_string = self.substrate_ws_url();

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
        let url = self.substrate_ws_url();
        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(url)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for ReviveDevNode {
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::types::TransactionRequest;
    use std::sync::{LazyLock, Mutex};

    use std::fs;

    use alloy::primitives::U256;

    use super::*;

    fn test_config() -> Test {
        Test::default()
    }

    fn new_node() -> (Test, ReviveDevNode) {
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
        let revive_dev_node_path = context.revive_dev_node.path.clone();
        let genesis = context.genesis.genesis().unwrap().clone();
        let mut node = ReviveDevNode::new(
            revive_dev_node_path,
            None,
            context.clone(),
            true,
            "".to_string(),
            "".to_string(),
        );
        node.init(genesis)
            .expect("Failed to initialize the node")
            .spawn_process()
            .expect("Failed to spawn the node process");
        (context, node)
    }

    fn shared_state() -> &'static (Test, ReviveDevNode) {
        static STATE: LazyLock<(Test, ReviveDevNode)> = LazyLock::new(new_node);
        &STATE
    }

    fn shared_node() -> &'static ReviveDevNode {
        &shared_state().1
    }

    #[tokio::test]
    #[ignore = "Ignored since it takes a long time to run"]
    async fn node_mines_simple_transfer_transaction_and_returns_receipt() {
        // Arrange
        let (context, node) = shared_state();

        let provider = node.provider().await.expect("Failed to create provider");

        let account_address = context.wallet.wallet().default_signer().address();
        let transaction = TransactionRequest::default()
            .to(account_address)
            .value(U256::from(100_000_000_000_000u128));

        // Act
        let mut pending_transaction = provider
            .send_transaction(transaction)
            .await
            .expect("Submission failed");
        pending_transaction.set_timeout(Some(Duration::from_secs(60)));

        // Assert
        let _ = pending_transaction
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
        let revive_dev_node_path = context.revive_dev_node.path.clone();
        let mut dummy_node = ReviveDevNode::new(
            revive_dev_node_path,
            None,
            context,
            true,
            "".to_string(),
            "".to_string(),
        );

        // Call `init()`
        dummy_node
            .init(serde_json::from_str(genesis_content).unwrap())
            .expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = dummy_node
            .directories
            .base_directory()
            .join(ReviveDevNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents = fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Substrate addresses derived from the Ethereum addresses are in the file
        let first_eth_addr = crate::helpers::eth_to_polkadot_address(
            &"90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap(),
        );
        let second_eth_addr = crate::helpers::eth_to_polkadot_address(
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
}
