use crate::internal_prelude::*;

/// A node implementation for Substrate based chains. Currently, this supports either substrate
/// or the revive-dev-node which is done by changing the path and some of the other arguments passed
/// to the command.
#[derive(Debug)]

pub struct ReviveDevNode {
    id: u32,
    revive_dev_node_process: ReviveDevNodeProcess,
    eth_rpc_process: EthRpcProcess,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    gas_filler: FallbackGasFiller,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,
    _directories: NodeDirectories,
}

impl ReviveDevNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasReviveDevNodeConfiguration
        + Clone,
        use_fallback_gas_filler: bool,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let wallet_config = context.as_wallet_configuration();
        let node_config = context.as_revive_dev_node_configuration();
        let rpc_config = context.as_eth_rpc_configuration();

        let id = NodeId::for_node("revive-dev-node");
        let directories = NodeDirectories::new(
            workdir_config.working_directory.as_path(),
            "revive-dev-node",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let chainspec_path = directories.base_directory().join("chainspec.json");

        let wallet = wallet_config.wallet();
        Self::init_chainspec(
            node_config.path.as_path(),
            &wallet,
            chainspec_path.as_path(),
        )
        .context("Failed to initialize the chainspec file")?;

        let revive_dev_node_process = ReviveDevNodeProcess::new(
            node_config.path.as_path(),
            chainspec_path,
            node_config.consensus.as_str(),
            directories.data_directory(),
            directories.logs_directory(),
            node_config.logging_level.as_str(),
            node_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn revive-dev-node"))?;

        let eth_rpc_process = EthRpcProcess::new(
            rpc_config.path.as_path(),
            directories.logs_directory(),
            revive_dev_node_process.url(),
            rpc_config.logging_level.as_str(),
            rpc_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?;

        Ok(Self {
            id: id.0,
            revive_dev_node_process,
            eth_rpc_process,
            wallet,
            nonce_manager: Default::default(),
            gas_filler: FallbackGasFiller::new().with_fallback_mechanism(use_fallback_gas_filler),
            provider: Default::default(),
            substrate_provider: Default::default(),
            _directories: directories,
        })
    }

    fn init_chainspec(
        binary_path: impl AsRef<Path>,
        wallet: &EthereumWallet,
        chainspec_path: impl AsRef<Path>,
    ) -> Result<()> {
        let chainspec =
            Self::chainspec(binary_path, wallet).context("Failed to create the chainspec")?;
        File::create(chainspec_path)
            .context("Failed to create the chainspec file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &chainspec)
                    .context("Failed to serialize chainspec to writer")
            })?;

        Ok(())
    }

    pub fn chainspec(binary_path: impl AsRef<Path>, wallet: &EthereumWallet) -> Result<Value> {
        static CACHED_BASE_CHAINSPECS: LazyLock<Arc<StdMutex<HashMap<PathBuf, Value>>>> =
            LazyLock::new(Default::default);

        let mut chainspec = match CACHED_BASE_CHAINSPECS
            .lock()
            .expect("poisoned")
            .entry(binary_path.as_ref().to_path_buf())
        {
            HashMapEntry::Occupied(entry) => entry.get().clone(),
            HashMapEntry::Vacant(entry) => {
                let chainspec = Command::new(binary_path.as_ref())
                    .arg("build-spec")
                    .arg("--chain")
                    .arg("dev")
                    .env_remove("RUST_LOG")
                    .run_and_get_output()
                    .context("Failed to build the chainspec")
                    .and_then(|output| {
                        serde_json::from_str::<Value>(&output.stdout)
                            .context("Failed to deserialize output as chainspec JSON")
                    })?;
                entry.insert(chainspec.clone());
                chainspec
            }
        };
        inject_wallet_balances(&mut chainspec, wallet)
            .context("Failed to add the pre-funded accounts")?;

        Ok(chainspec)
    }

    fn provider(&self) -> StaticFuture<Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_string();
        let gas_filler = self.gas_filler;
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
}

impl NodeApi for ReviveDevNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.eth_rpc_process.url()
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
    ) -> StaticFuture<Result<TxHash>> {
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

    fn provider(&self) -> revive_dt_common::futures::StaticFuture<Result<DynProvider>> {
        Self::provider(self)
            .map(|provider| provider.map(|provider| provider.erased()))
            .boxed()
    }

    fn substrate_provider(
        &self,
    ) -> Option<revive_dt_common::futures::StaticFuture<Result<OnlineClient<PolkadotConfig>>>>
    {
        let provider = self.substrate_provider.clone();
        let connection_string = self.revive_dev_node_process.url().to_string();

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
    ) -> Option<StaticFuture<Result<subxt::backend::rpc::RpcClient>>> {
        let url = self.revive_dev_node_process.url().to_string();
        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(url)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for ReviveDevNode {
    fn spawn(&mut self, _: Genesis) -> Result<()> {
        Ok(())
    }
}
