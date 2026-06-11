use alloy::consensus::TxEnvelope;

use crate::internal_prelude::*;

type AlloyProvider = FillProvider<
    JoinFill<
        JoinFill<JoinFill<JoinFill<Identity, GasFiller>, ChainIdFiller>, DelayedNonceFiller>,
        WalletFiller<Arc<EthereumWallet>>,
    >,
    RootProvider,
>;

pub type RuntimeSubxtBlock = GenericBlock<GenericHeader<u32, BlakeTwo256>, OpaqueExtrinsic>;
type OnlineSubxtBlock = SubxtBlock<PolkadotConfig, OnlineClient<PolkadotConfig>>;

pub struct NodeConnector {
    eth_providers: SingleOrPool<AlloyProvider>,
    substrate_providers: Option<SubstrateProviders>,
    /// The raw substrate RPC client(s), sharing connections with
    /// `substrate_providers`. Used to submit transactions via the raw
    /// `author_submitExtrinsic` method (fire-and-forget, no status
    /// subscription).
    submission_rpc: Option<SingleOrPool<SubxtRpcClient>>,
    config: NodeConnectorConfiguration,
    /* State */
    latest_finalized_block: Arc<RwLock<Option<BlockPair>>>,
    finalized_block_broadcast: BroadcastSender<BlockPair>,
    blocks_by_number: AsyncHashMap<u64, BlockPair>,
    indexed_transactions: AsyncHashMap<TxHash, IndexedTransactionInformation>,
    tx_hash_to_receipt_mapping: AsyncHashMap<TxHash, Arc<TransactionReceipt>>,
    block_hashes_to_receipt_mapping: AsyncHashMap<BlockHash, Vec<Arc<TransactionReceipt>>>,
    filled_transactions: Arc<DashMap<TxHash, TxEnvelope>>,
    eth_rpc_latest_block_number_tx: WatchSender<u64>,
    /* Locks & Gates */
    submission_locks: DashMap<Address, Arc<Mutex<()>>>,
    substrate_submission_semaphore: Option<Arc<Semaphore>>,
    /* Auxiliary */
    node: Box<dyn NodeConfiguration + Send + Sync>,
}

impl NodeConnector {
    pub fn new<N: NodeConfiguration + Send + Sync + 'static>(
        node: N,
        wallet: Arc<EthereumWallet>,
        config: NodeConnectorConfiguration,
    ) -> StaticFuture<Result<Self>> {
        let config = config.resolve(node.configurations());

        let substrate_submission_semaphore = config
            .substrate_provider_configuration
            .as_ref()
            .and_then(|config| config.submission_concurrency_configuration.as_ref())
            .and_then(|config| match config {
                ProviderConcurrencyConfiguration::Disabled => None,
                ProviderConcurrencyConfiguration::SemaphoreBasedLimiter { permits } => {
                    Some(Arc::new(Semaphore::new(*permits)))
                }
            });

        let eth_provider_urls = node.eth_provider_url();
        let substrate_rpc_urls = node.substrate_provider_url();
        let nonce_filler = DelayedNonceFiller::default();

        let eth_providers_future =
            Self::eth_providers_future(eth_provider_urls, config, wallet, nonce_filler);
        let substrate_providers_future = Self::substrate_providers_future(substrate_rpc_urls);

        Box::pin(async move {
            let eth_providers = eth_providers_future
                .await
                .context("Failed to get the eth providers")?;
            let (substrate_providers, submission_rpc) = match substrate_providers_future {
                Some(substrate_providers_future) => {
                    let (online_providers, rpc_clients) = substrate_providers_future
                        .await
                        .context("Failed to get the substrate providers")?;
                    (
                        Some(SubstrateProviders::new(online_providers)),
                        Some(rpc_clients),
                    )
                }
                None => (None, None),
            };

            let unresolved_finalized_blocks_broadcast_tx =
                Self::start_unresolved_finalized_blocks_broadcaster(
                    eth_providers.clone(),
                    substrate_providers.clone(),
                    &config,
                );
            let finalized_blocks_broadcast_tx = Self::start_resolved_finalized_blocks_broadcaster(
                unresolved_finalized_blocks_broadcast_tx.subscribe(),
                substrate_providers.clone(),
            );
            let indexed_transactions = AsyncHashMap::new();
            Self::start_transaction_indexer(
                finalized_blocks_broadcast_tx.subscribe(),
                indexed_transactions.clone(),
            );
            let latest_finalized_block = Arc::new(RwLock::new(None));
            Self::start_latest_finalized_block_updater(
                latest_finalized_block.clone(),
                finalized_blocks_broadcast_tx.subscribe(),
            );
            let blocks_by_number = AsyncHashMap::new();
            Self::start_block_by_number_indexer(
                blocks_by_number.clone(),
                finalized_blocks_broadcast_tx.subscribe(),
            );
            let tx_hash_to_receipt_mapping = AsyncHashMap::new();
            let block_hashes_to_receipt_mapping = AsyncHashMap::new();
            Self::start_receipt_provider(
                finalized_blocks_broadcast_tx.subscribe(),
                eth_providers.clone(),
                substrate_providers.clone(),
                tx_hash_to_receipt_mapping.clone(),
                block_hashes_to_receipt_mapping.clone(),
            );

            let eth_rpc_latest_block_number_tx =
                Self::start_eth_rpc_latest_block_number(eth_providers.clone())
                    .await
                    .context("Failed to start the eth-rpc latest block number poller")?;

            Ok(Self {
                eth_providers,
                substrate_providers,
                submission_rpc,
                config,
                latest_finalized_block,
                finalized_block_broadcast: finalized_blocks_broadcast_tx,
                blocks_by_number,
                indexed_transactions,
                submission_locks: DashMap::new(),
                tx_hash_to_receipt_mapping,
                block_hashes_to_receipt_mapping,
                filled_transactions: Arc::new(DashMap::new()),
                eth_rpc_latest_block_number_tx,
                substrate_submission_semaphore,
                node: Box::new(node),
            })
        })
    }

    pub fn node_id(&self) -> usize {
        self.node.id()
    }

    pub fn evm_version(&self) -> EVMVersion {
        self.node.evm_version()
    }

    pub fn chain_id(&self) -> StaticFuture<Result<u64>> {
        let provider = self.eth_providers.clone();
        Box::pin(async move {
            provider
                .get_chain_id()
                .await
                .context("Failed to get the chain id")
        })
    }

    pub fn execute_transaction(
        &self,
        tx: TransactionRequest,
    ) -> StaticFuture<Result<Arc<TransactionReceipt>>> {
        let substrate_provider = self.substrate_providers.clone();
        let indexed_transactions = self.indexed_transactions.clone();
        let tx_hash_to_receipt_mapping = self.tx_hash_to_receipt_mapping.clone();
        let eth_rpc_latest_block_number_rx = self.eth_rpc_latest_block_number_tx.subscribe();
        self.send_transaction(tx)
            .and_then(move |tx_hash| {
                Self::get_receipt_internal(
                    eth_rpc_latest_block_number_rx,
                    substrate_provider,
                    indexed_transactions,
                    tx_hash_to_receipt_mapping,
                    tx_hash,
                )
            })
            .boxed()
    }

    pub fn get_transaction(&self, tx_hash: TxHash) -> Result<TxEnvelope> {
        self.filled_transactions
            .get(&tx_hash)
            .map(|entry| entry.value().clone())
            .context("No filled transaction is stored for the given hash")
    }

    pub fn send_transaction(&self, mut tx: TransactionRequest) -> StaticFuture<Result<TxHash>> {
        let config = self
            .config
            .hooks
            .as_ref()
            .and_then(|config| config.pre_submission_hook.as_ref());
        match config {
            Some(PreSubmissionHook::MaxGasPrice) => tx = tx.gas_price(u128::MAX),
            Some(PreSubmissionHook::Disabled) | None => {}
        }

        let config = self
            .config
            .behaviors
            .as_ref()
            .and_then(|config| config.submission_behavior)
            .unwrap_or(SubmissionBehavior::UseDefaultForPlatform);

        match (config, self.substrate_providers.as_ref()) {
            (SubmissionBehavior::UseDefaultForPlatform, None)
            | (SubmissionBehavior::UseEthRpc, ..) => self.send_transaction_evm(tx),
            (
                SubmissionBehavior::UseDefaultForPlatform | SubmissionBehavior::UseSubstrateRpc,
                Some(substrate_provider),
            ) => self.send_transaction_substrate(tx, substrate_provider.clone()),
            (SubmissionBehavior::UseSubstrateRpcAndAwaitValidation, Some(substrate_provider)) => {
                let submission_rpc = self
                    .submission_rpc
                    .clone()
                    .expect("submission rpc is built alongside the substrate providers");
                self.send_transaction_and_await_validation_substrate(
                    tx,
                    substrate_provider.clone(),
                    submission_rpc,
                )
            }
            (
                SubmissionBehavior::UseSubstrateRpc
                | SubmissionBehavior::UseSubstrateRpcAndAwaitValidation,
                None,
            ) => Box::pin(ready(Err(anyhow!(
                "Can not use the substrate provider on a non-substrate chain"
            )))),
        }
    }

    fn send_transaction_evm(&self, tx: TransactionRequest) -> StaticFuture<Result<TxHash>> {
        let provider = self.eth_providers.clone();
        self.send_transaction_internal(tx, |tx| async move {
            provider
                .send_tx_envelope(tx)
                .await
                .context("Failed to send transaction through the provider")
                .map(|builder| *builder.tx_hash())
        })
    }

    fn send_transaction_substrate(
        &self,
        tx: TransactionRequest,
        substrate_provider: SubstrateProviders,
    ) -> StaticFuture<Result<TxHash>> {
        let submission_mutex = tx
            .from
            .map(|addr| self.submission_locks.entry(addr).or_default().clone());
        let submission_semaphore = self.substrate_submission_semaphore.clone();
        self.send_transaction_internal(tx, |tx| async move {
            let _guard = match submission_mutex {
                Some(mutex) => Some(mutex.lock_owned().await),
                None => None,
            };
            let _permit = match submission_semaphore {
                Some(semaphore) => Some(
                    semaphore
                        .acquire_owned()
                        .await
                        .context("Failed to acquire permit"),
                ),
                None => None,
            };

            let ethereum_tx_hash = *tx.tx_hash();
            let payload = tx.encoded_2718();
            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec())
                .unvalidated();
            let substrate_hash = substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create the unsigned transaction")?
                .submit()
                .await
                .context("Failed to submit the transaction through subxt")?;

            info!(
                evm_hash = %ethereum_tx_hash,
                ?substrate_hash,
                "Submitting a substrate transaction"
            );

            Ok(ethereum_tx_hash)
        })
    }

    fn send_transaction_and_await_validation_substrate(
        &self,
        tx: TransactionRequest,
        substrate_provider: SubstrateProviders,
        submission_rpc: SingleOrPool<SubxtRpcClient>,
    ) -> StaticFuture<Result<TxHash>> {
        let submission_mutex = tx
            .from
            .map(|addr| self.submission_locks.entry(addr).or_default().clone());
        let submission_semaphore = self.substrate_submission_semaphore.clone();
        let indexed_transactions = self.indexed_transactions.clone();
        self.send_transaction_internal(tx, |tx| async move {
            let _guard = match submission_mutex {
                Some(mutex) => Some(mutex.lock_owned().await),
                None => None,
            };
            let _permit = match submission_semaphore {
                Some(semaphore) => Some(
                    semaphore
                        .acquire_owned()
                        .await
                        .context("Failed to acquire permit"),
                ),
                None => None,
            };

            let ethereum_tx_hash = *tx.tx_hash();
            let payload = tx.encoded_2718();
            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec())
                .unvalidated();
            let substrate_transaction = substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create substrate transaction")?;
            let substrate_tx_hash = substrate_transaction.hash();

            debug!(
                evm_hash = %ethereum_tx_hash,
                substrate_hash = ?substrate_tx_hash,
                "Create a substrate transaction, but didn't yet submit it"
            );

            let submitter = LegacyRpcMethods::<PolkadotConfig>::new(submission_rpc.item().clone());
            match submitter
                .author_submit_extrinsic(substrate_transaction.encoded())
                .await
            {
                Ok(_) => {}
                // Ignore already submitted errors
                Err(RpcsError::User(user)) if user.code == 1013 => {
                    warn!("Encountered an 'Already Submitted' error; continuing")
                }
                Err(error) => return Err(error).context("Failed to submit transaction"),
            }
            // Await validation & inclusion
            let _ = indexed_transactions.get(ethereum_tx_hash).await;

            Ok(ethereum_tx_hash)
        })
    }

    fn send_transaction_internal<Func, Fut>(
        &self,
        tx: TransactionRequest,
        submission_future: Func,
    ) -> StaticFuture<Result<TxHash>>
    where
        Func: FnOnce(TxEnvelope) -> Fut + Send + 'static,
        Fut: Future<Output = Result<TxHash>> + Send + 'static,
    {
        self.prepare_transaction(tx)
            .map(|result| result.context("Transaction preparation failed"))
            .and_then(|envelope| {
                submission_future(envelope)
                    .map(|result| result.context("Transaction submission failed"))
            })
            .boxed()
    }

    fn prepare_transaction(&self, mut tx: TransactionRequest) -> StaticFuture<Result<TxEnvelope>> {
        let config = self
            .config
            .hooks
            .as_ref()
            .and_then(|config| config.pre_submission_hook.as_ref());
        match config {
            Some(PreSubmissionHook::MaxGasPrice) => tx.set_gas_price(u128::MAX),
            Some(PreSubmissionHook::Disabled) | None => {}
        };

        let gas_estimate = self.estimate_gas(tx.clone());

        let provider = self.eth_providers.clone();
        let transactions_map = self.filled_transactions.clone();
        Box::pin(async move {
            let gas_estimate = gas_estimate
                .await
                .context("Failed to get the gas estimate of the transaction")?;
            tx.set_gas_limit(gas_estimate * 120 / 100);
            let filled_transaction = provider
                .fill(tx)
                .await
                .context("Transaction filling failed")?
                .try_into_envelope()
                .expect("qed; filled transactions must be envelopes");
            transactions_map.insert(*filled_transaction.tx_hash(), filled_transaction.clone());
            Ok(filled_transaction)
        })
    }

    pub fn get_receipt(&self, tx_hash: TxHash) -> StaticFuture<Result<Arc<TransactionReceipt>>> {
        Self::get_receipt_internal(
            self.eth_rpc_latest_block_number_tx.subscribe(),
            self.substrate_providers.clone(),
            self.indexed_transactions.clone(),
            self.tx_hash_to_receipt_mapping.clone(),
            tx_hash,
        )
    }

    pub fn get_block_receipts(
        &self,
        block_hash: BlockHash,
    ) -> StaticFuture<Vec<Arc<TransactionReceipt>>> {
        self.block_hashes_to_receipt_mapping.get(block_hash)
    }

    fn get_receipt_internal(
        mut eth_rpc_latest_block_number_rx: WatchReceiver<u64>,
        substrate_provider: Option<SubstrateProviders>,
        indexed_transactions: AsyncHashMap<TxHash, IndexedTransactionInformation>,
        tx_hash_to_receipt_mapping: AsyncHashMap<TxHash, Arc<TransactionReceipt>>,
        tx_hash: TxHash,
    ) -> StaticFuture<Result<Arc<TransactionReceipt>>> {
        Box::pin(async move {
            let block = indexed_transactions.get(tx_hash).await;

            if substrate_provider.is_some() {
                let target_block_number = block.block_pair.evm_block.number();
                timeout(
                    Duration::from_secs(5 * 60),
                    eth_rpc_latest_block_number_rx
                        .wait_for(|block_number| *block_number >= target_block_number),
                )
                .await
                .context("Timed out waiting for the eth-rpc block to advance")?
                .context("Failed to wait for eth-rpc block to advance")?;
            }

            Ok(tx_hash_to_receipt_mapping.get(tx_hash).await)
        })
    }

    pub fn code_upload_transaction(&self, code: impl AsRef<[u8]>) -> Result<TransactionRequest> {
        const RUNTIME_PALLET_ADDRESS: Address =
            address!("0x6d6f646c70792f70616464720000000000000000");

        let provider = self.substrate_providers.clone();
        let provider = provider
            .context("Code upload operations are only supported on substrate based chains")?;
        let metadata = provider.metadata();
        let upload_call = dynamic::tx(
            "Revive",
            "upload_code",
            vec![
                dynamic::Value::from_bytes(code),
                dynamic::Value::u128(u128::MAX),
            ],
        );
        let encoded_payload = upload_call
            .encode_call_data(&metadata)
            .context("Failed to encode the upload code payload")?;

        Ok(TransactionRequest::default()
            .to(RUNTIME_PALLET_ADDRESS)
            .input(encoded_payload.into()))
    }

    pub fn estimate_gas(&self, tx: TransactionRequest) -> StaticFuture<Result<u64>> {
        match self.substrate_providers.as_ref() {
            Some(substrate_provider)
                if substrate_provider.available_estimation_methods().has_any() =>
            {
                self.estimate_gas_substrate(tx, substrate_provider)
            }
            Some(substrate_provider) => {
                warn!(
                    ?substrate_provider.available_estimation_methods,
                    "No ReviveApi gas estimation method is available; falling back to eth-rpc"
                );
                self.estimate_gas_evm(tx)
            }
            None => self.estimate_gas_evm(tx),
        }
    }

    fn estimate_gas_evm(&self, tx: TransactionRequest) -> StaticFuture<Result<u64>> {
        self.eth_providers
            .clone()
            .estimate_gas(tx)
            .into_future()
            .map(|res| res.context("Failed to get the gas estimate"))
            .boxed()
    }

    fn estimate_gas_substrate(
        &self,
        tx: TransactionRequest,
        substrate_provider: &SubstrateProviders,
    ) -> StaticFuture<Result<u64>> {
        let provider = substrate_provider.clone();
        let available_methods = substrate_provider.available_estimation_methods();

        Box::pin(async move {
            let runtime_api = provider
                .runtime_api()
                .at_latest()
                .await
                .context("Failed to get the runtime API at the latest finalized block")?;
            let tx = transaction_request_to_revive_transaction(tx)?;

            if available_methods.estimate_gas {
                let payload = revive_metadata::apis()
                    .revive_api()
                    .eth_estimate_gas(tx.into(), pallet_revive::DryRunConfig::default().into())
                    .unvalidated();
                let gas = runtime_api
                    .call(payload)
                    .await
                    .context("Failed to call ReviveApi_eth_estimate_gas")?
                    .map_err(|err| anyhow!("ReviveApi_eth_estimate_gas failed: {:?}", err.0))?
                    .0;
                Ok(gas.try_into().expect("qed; gas in revive must fit in u64"))
            } else if available_methods.eth_transact_with_config {
                let payload = revive_metadata::apis()
                    .revive_api()
                    .eth_transact_with_config(
                        tx.into(),
                        pallet_revive::DryRunConfig::default().into(),
                    )
                    .unvalidated();
                let gas = runtime_api
                    .call(payload)
                    .await
                    .context("Failed to call ReviveApi_eth_transact_with_config")?
                    .map_err(|err| {
                        anyhow!("ReviveApi_eth_transact_with_config failed: {:?}", err.0)
                    })?
                    .eth_gas;
                Ok(gas.try_into().expect("qed; gas in revive must fit in u64"))
            } else {
                let payload = revive_metadata::apis()
                    .revive_api()
                    .eth_transact(tx.into())
                    .unvalidated();
                let gas = runtime_api
                    .call(payload)
                    .await
                    .context("Failed to call ReviveApi_eth_transact")?
                    .map_err(|err| anyhow!("ReviveApi_eth_transact failed: {:?}", err.0))?
                    .eth_gas;
                Ok(gas.try_into().expect("qed; gas in revive must fit in u64"))
            }
        })
    }

    pub fn subscribe_to_finalized_blocks(&self) -> StaticStream<BlockPair> {
        BroadcastStream::new(self.finalized_block_broadcast.subscribe())
            .filter_map(|block| async move { block.ok() })
            .boxed()
    }

    pub fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> StaticFuture<Result<GethTrace>> {
        match self.substrate_providers.as_ref() {
            Some(substrate_provider) => {
                self.trace_transaction_substrate(tx_hash, trace_options, substrate_provider)
            }
            None => self.trace_transaction_evm(tx_hash, trace_options),
        }
    }

    pub fn trace_call(
        &self,
        tx: TransactionRequest,
        block: BlockPair,
        trace_options: GethDebugTracingOptions,
    ) -> StaticFuture<Result<GethTrace>> {
        match self.substrate_providers.as_ref() {
            Some(substrate_provider) => {
                self.trace_call_substrate(tx, block, trace_options, substrate_provider)
            }
            None => self.trace_call_evm(tx, block, trace_options),
        }
    }

    fn trace_call_evm(
        &self,
        tx: TransactionRequest,
        block: BlockPair,
        trace_options: GethDebugTracingOptions,
    ) -> StaticFuture<Result<GethTrace>> {
        let provider = self.eth_providers.clone();

        Box::pin(async move {
            provider
                .debug_trace_call(
                    tx,
                    BlockId::hash(block.evm_block.hash()),
                    GethDebugTracingCallOptions::new(trace_options),
                )
                .await
                .context("Failed to get the call trace")
        })
    }

    fn trace_call_substrate(
        &self,
        tx: TransactionRequest,
        block: BlockPair,
        trace_options: GethDebugTracingOptions,
        substrate_provider: &SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> StaticFuture<Result<GethTrace>> {
        let provider = substrate_provider.clone();

        Box::pin(async move {
            let substrate_block_hash = block
                .substrate_block
                .as_ref()
                .context("No substrate block available for trace_call")?
                .online_block
                .hash();

            let tx = transaction_request_to_revive_transaction(tx)?;
            let tracer_type = geth_trace_options_to_revive_tracer_type(trace_options)?;
            let payload = revive_metadata::apis()
                .revive_api()
                .trace_call(tx.into(), tracer_type.into())
                .unvalidated();
            let trace = provider
                .runtime_api()
                .at(substrate_block_hash)
                .call(payload)
                .await
                .context("Failed to get the call trace")?
                .map_err(|err| anyhow!("Failed to get the call trace: {err:?}"))?
                .0;

            revive_trace_to_geth_trace(trace)
        })
    }

    fn trace_transaction_evm(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> StaticFuture<Result<GethTrace>> {
        let inclusion_future = self.indexed_transactions.get(tx_hash);
        let provider = self.eth_providers.clone();

        Box::pin(async move {
            let _ = inclusion_future.await;
            provider
                .debug_trace_transaction(tx_hash, trace_options)
                .await
                .context("Failed to get the transaction trace")
        })
    }

    fn trace_transaction_substrate(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
        substrate_provider: &SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> StaticFuture<Result<GethTrace>> {
        let provider = substrate_provider.clone();
        let inclusion_future = self.indexed_transactions.get(tx_hash);

        Box::pin(async move {
            let indexed_transaction = inclusion_future.await;
            let substrate_block_information = indexed_transaction
                .block_pair
                .substrate_block
                .as_ref()
                .expect("qed; this is a substrate transaction");
            let extrinsic_index = indexed_transaction
                .extrinsic_index
                .expect("qed; this is a substrate transaction");
            let extrinsic_index = extrinsic_index as u32;

            let parent_hash = substrate_block_information.runtime_block.header.parent_hash;
            let block = substrate_block_information.runtime_block.clone();

            let tracer_type = geth_trace_options_to_revive_tracer_type(trace_options)?;
            let payload = revive_metadata::apis()
                .revive_api()
                .trace_tx(block.into(), extrinsic_index, tracer_type.into())
                .unvalidated();
            let trace = provider
                .runtime_api()
                .at(parent_hash)
                .call(payload)
                .await
                .context("Failed to get the transaction trace")?
                .context("Failed to get the transaction trace")?
                .0;

            revive_trace_to_geth_trace(trace)
        })
    }

    pub fn balance_of(&self, address: Address) -> StaticFuture<Result<U256>> {
        match self.substrate_providers.as_ref() {
            Some(substrate_provider) => self.balance_of_substrate(address, substrate_provider),
            None => self.balance_of_evm(address),
        }
    }

    fn balance_of_evm(&self, address: Address) -> StaticFuture<Result<U256>> {
        let provider = self.eth_providers.clone();
        Box::pin(async move {
            provider
                .get_balance(address)
                .finalized()
                .await
                .context("Failed to get the balance of the account")
        })
    }

    fn balance_of_substrate(
        &self,
        address: Address,
        provider: &SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> StaticFuture<Result<U256>> {
        let provider = provider.clone();
        let latest_block = self.latest_finalized_block.clone();

        Box::pin(async move {
            let runtime_api = match latest_block.read().await.as_ref() {
                Some(latest_block) => provider.runtime_api().at(latest_block
                    .substrate_block
                    .as_ref()
                    .expect("qed; this is a substrate node")
                    .online_block
                    .hash()),
                None => provider
                    .runtime_api()
                    .at_latest()
                    .await
                    .context("Failed to get the runtime API at the latest finalized block")?,
            };
            let payload = revive_metadata::apis()
                .revive_api()
                .balance(pallet_revive::H160(address.0.0).into())
                .unvalidated();
            let balance = runtime_api
                .call(payload)
                .await
                .context("Failed to get the balance")?;
            Ok(U256::from_limbs_slice(&balance.0.0))
        })
    }

    pub fn block(&self, number: u64) -> StaticFuture<BlockPair> {
        self.blocks_by_number.get(number)
    }

    /// Returns the finalized block that contains `tx_hash`.
    pub fn transaction_block_pair(&self, tx_hash: TxHash) -> StaticFuture<BlockPair> {
        let inclusion_future = self.indexed_transactions.get(tx_hash);
        Box::pin(async move { inclusion_future.await.block_pair })
    }

    pub fn latest_finalized_block(&self) -> StaticFuture<Result<BlockPair>> {
        let mut finalized_blocks = self.finalized_block_broadcast.subscribe();
        let latest_block = self.latest_finalized_block.clone();

        Box::pin(async move {
            if let Some(block) = latest_block.read().await.as_ref() {
                return Ok(block.clone());
            }

            finalized_blocks
                .recv()
                .await
                .context("Failed to receive the latest finalized block")
        })
    }

    pub fn pre_dispatch_weights(
        &self,
        block_hash: [u8; 32],
        payload: Vec<u8>,
    ) -> StaticFuture<Result<Weight>> {
        let provider = self.substrate_providers.clone();
        Box::pin(async move {
            let provider = provider.context("No substrate provider available")?;
            let encoded_args = payload.encode();
            let encoded_result = provider
                .runtime_api()
                .at(H256(block_hash))
                .call_raw(
                    "ReviveApi_eth_pre_dispatch_weight",
                    Some(encoded_args.as_slice()),
                )
                .await
                .context("Failed to get the pre-dispatch weights")?;

            let result =
                StdResult::<Weight, EthTransactError>::decode(&mut encoded_result.as_slice())
                    .context("Failed to decode pre-dispatch weight result")?;

            result.map_err(|err| anyhow!("pre-dispatch weight returned an error: {err:?}"))
        })
    }

    fn eth_providers_future(
        eth_provider_urls: NodeUrlCollection<'_>,
        config: NodeConnectorConfiguration,
        wallet: Arc<EthereumWallet>,
        nonce_filler: DelayedNonceFiller,
    ) -> StaticFuture<Result<SingleOrPool<AlloyProvider>>> {
        match eth_provider_urls {
            NodeUrlCollection { ipc: Some(url), .. }
            | NodeUrlCollection {
                ipc: None,
                ws: None,
                http: Some(url),
            } => new_alloy_provider(url, config, wallet.clone(), nonce_filler)
                .map_ok(SingleOrPool::Single)
                .boxed() as StaticFuture<_>,
            NodeUrlCollection {
                ipc: None,
                ws: Some(url),
                ..
            } => try_join_all((0..10).map(move |_| {
                new_alloy_provider(url.as_ref(), config, wallet.clone(), nonce_filler.clone())
            }))
            .map_ok(Pool::new_unchecked)
            .map_ok(SingleOrPool::Pool)
            .boxed() as StaticFuture<_>,
            NodeUrlCollection {
                ipc: None,
                ws: None,
                http: None,
            } => Box::pin(ready(Err(anyhow!(
                "Node didn't provide any URLs for the eth rpc"
            )))),
        }
    }

    #[allow(clippy::type_complexity)]
    fn substrate_providers_future(
        substrate_provider_urls: Option<NodeUrlCollection<'_>>,
    ) -> Option<
        StaticFuture<
            Result<(
                SingleOrPool<OnlineClient<PolkadotConfig>>,
                SingleOrPool<SubxtRpcClient>,
            )>,
        >,
    > {
        match substrate_provider_urls? {
            NodeUrlCollection { ipc: Some(url), .. }
            | NodeUrlCollection {
                ipc: None,
                ws: None,
                http: Some(url),
            } => {
                let url = url.to_string();
                Some(
                    new_substrate_client(url)
                        .map_ok(|(online, rpc)| {
                            (SingleOrPool::Single(online), SingleOrPool::Single(rpc))
                        })
                        .boxed() as StaticFuture<_>,
                )
            }
            NodeUrlCollection {
                ipc: None,
                ws: Some(url),
                ..
            } => {
                let url = url.to_string();
                Some(
                    try_join_all((0..10).map(move |_| new_substrate_client(url.clone())))
                        .map_ok(|pairs| {
                            let (online, rpc): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
                            (
                                SingleOrPool::Pool(Pool::new_unchecked(online)),
                                SingleOrPool::Pool(Pool::new_unchecked(rpc)),
                            )
                        })
                        .boxed() as StaticFuture<_>,
                )
            }
            NodeUrlCollection {
                ipc: None,
                ws: None,
                http: None,
            } => None,
        }
    }

    pub fn inclusion_future(&self, tx_hash: TxHash) -> StaticFuture<()> {
        Box::pin(self.indexed_transactions.get(tx_hash).map(|_| ()))
    }

    fn start_unresolved_finalized_blocks_broadcaster(
        provider: SingleOrPool<AlloyProvider>,
        substrate_provider: Option<SubstrateProviders>,
        config: &NodeConnectorConfiguration,
    ) -> BroadcastSender<UnresolvedBlockPair> {
        let (tx, _) = broadcast_channel::<UnresolvedBlockPair>(2048);
        let task = match substrate_provider {
            Some(provider) => Self::start_unresolved_finalized_blocks_broadcaster_substrate(
                provider,
                tx.clone(),
                config,
            ),
            None => Self::start_unresolved_finalized_blocks_broadcaster_evm(provider, tx.clone()),
        };
        spawn(task);
        tx
    }

    fn start_unresolved_finalized_blocks_broadcaster_evm(
        provider: SingleOrPool<AlloyProvider>,
        tx: BroadcastSender<UnresolvedBlockPair>,
    ) -> StaticFuture<Result<()>> {
        Box::pin(async move {
            loop {
                let mut subscription = provider
                    .subscribe_full_blocks()
                    .hashes()
                    .into_stream()
                    .await?;
                while let Some(Ok(block)) = subscription.next().await {
                    let block_pair = UnresolvedBlockPair {
                        observation_time: SystemTime::now(),
                        evm_block: Arc::new(block),
                        substrate_block: None,
                    };
                    let _ = tx.send(block_pair);
                }

                warn!("EVM block subscription ended; resubscribing");
            }
        })
    }

    fn start_unresolved_finalized_blocks_broadcaster_substrate(
        provider: SubstrateProviders,
        tx: BroadcastSender<UnresolvedBlockPair>,
        config: &NodeConnectorConfiguration,
    ) -> StaticFuture<Result<()>> {
        let block_observation_times =
            Arc::<RwLock<HashMap<H256, SystemTime>>>::new(Default::default());

        let config = config
            .block_provisioning_behavior
            .as_ref()
            .and_then(|config| config.subscription_kind.as_ref());
        let subscription_kind = match config {
            Some(BlockProvisioningSubscriptionKind::BestBlocks) => {
                BlockProvisioningSubscriptionKind::BestBlocks
            }
            None | Some(BlockProvisioningSubscriptionKind::FinalizedBlocks) => {
                BlockProvisioningSubscriptionKind::FinalizedBlocks
            }
        };

        let block_observer: StaticFuture<Result<()>> = {
            let provider = provider.clone();
            let block_observation_times = block_observation_times.clone();
            Box::pin(async move {
                loop {
                    let mut subscription = provider
                        .blocks()
                        .subscribe_all()
                        .await
                        .context("Failed to subscribe to all blocks")?;
                    while let Some(Ok(block)) = subscription.next().await {
                        let time = SystemTime::now();
                        block_observation_times
                            .write()
                            .await
                            .insert(block.hash(), time);
                    }
                    warn!("Substrate block observation subscription ended; resubscribing");
                }
            })
        };

        let block_broadcaster: StaticFuture<Result<()>> = {
            let provider = provider.clone();
            let block_observation_time = block_observation_times.clone();
            Box::pin(async move {
                loop {
                    let subscription = match subscription_kind {
                        BlockProvisioningSubscriptionKind::BestBlocks => {
                            provider.blocks().subscribe_best().boxed()
                        }
                        BlockProvisioningSubscriptionKind::FinalizedBlocks => {
                            provider.blocks().subscribe_finalized().boxed()
                        }
                    };
                    let mut subscription = subscription
                        .await
                        .context("Failed to subscribe to substrate blocks")?;

                    while let Some(substrate_block) = subscription.next().await {
                        let substrate_block = match substrate_block {
                            Ok(substrate_block) => substrate_block,
                            Err(err) => {
                                warn!(
                                    ?err,
                                    "Finalized substrate block subscription failed; resubscribing"
                                );
                                break;
                            }
                        };
                        let runtime_api = substrate_block
                            .runtime_api()
                            .await
                            .context("Failed to get the runtime API")?;

                        let payload = revive_metadata::apis()
                            .revive_api()
                            .eth_block()
                            .unvalidated();
                        let eth_block = runtime_api
                            .call(payload)
                            .await
                            .context("Failed to call the eth_block runtime API")?;
                        let eth_block_json = serde_json::to_value(eth_block.0)
                            .expect("qed; pallet-revive eth block serializes to JSON");
                        let evm_block = serde_json::from_value::<EvmBlock>(eth_block_json).expect(
                            "qed; pallet-revive and alloy block JSON formats are identical",
                        );

                        let block_pair = UnresolvedBlockPair {
                            observation_time: block_observation_time
                                .read()
                                .await
                                .get(&substrate_block.hash())
                                .copied()
                                .unwrap_or(SystemTime::now()),
                            evm_block: Arc::new(evm_block),
                            substrate_block: Some(Arc::new(substrate_block)),
                        };
                        let _ = tx.send(block_pair);
                    }

                    warn!("Finalized substrate block subscription ended; resubscribing");
                }
            })
        };
        futures::future::try_join(block_observer, block_broadcaster)
            .map_ok(|_| ())
            .boxed()
    }

    fn start_resolved_finalized_blocks_broadcaster(
        mut rx: BroadcastReceiver<UnresolvedBlockPair>,
        substrate_provider: Option<SubstrateProviders>,
    ) -> BroadcastSender<BlockPair> {
        let (tx, _) = broadcast_channel::<BlockPair>(2048);
        let task_tx = tx.clone();
        spawn(async move {
            let limits = match substrate_provider.as_ref() {
                Some(substrate_provider) => {
                    match Self::substrate_block_limits(substrate_provider) {
                        Ok(limits) => Some(limits),
                        Err(err) => {
                            error!(?err, "Failed to resolve substrate block limits");
                            return;
                        }
                    }
                }
                None => None,
            };

            while let Ok(block) = rx.recv().await {
                match Self::resolve_block_pair(block, limits).await {
                    Ok(block) => {
                        let _ = task_tx.send(block);
                    }
                    Err(err) => error!(?err, "Failed to resolve finalized block"),
                }
            }
        });
        tx
    }

    async fn resolve_block_pair(
        block: UnresolvedBlockPair,
        limits: Option<Weight>,
    ) -> Result<BlockPair> {
        let substrate_block = match block.substrate_block.as_ref() {
            Some(substrate_block) => Some(
                Self::resolve_substrate_block(substrate_block.clone(), limits)
                    .await
                    .context("Failed to resolve substrate block")?,
            ),
            None => None,
        };

        Ok(BlockPair {
            observation_time: block.observation_time,
            evm_block: block.evm_block,
            substrate_block: substrate_block.map(Arc::new),
        })
    }

    async fn resolve_substrate_block(
        online_block: Arc<OnlineSubxtBlock>,
        limits: Option<Weight>,
    ) -> Result<SubstrateBlockInformation> {
        let extrinsics = online_block
            .extrinsics()
            .await
            .context("Failed to get the substrate block extrinsics")?;

        let header = GenericHeader::<u32, BlakeTwo256>::decode(
            &mut online_block.header().encode().as_slice(),
        )
        .context("Failed to decode the substrate block header into the runtime header type")?;
        let runtime_extrinsics = extrinsics
            .iter()
            .map(|extrinsic| {
                OpaqueExtrinsic::from_bytes(extrinsic.bytes())
                    .context("Failed to decode the extrinsic into an opaque extrinsic")
            })
            .collect::<Result<Vec<_>>>()?;
        let eth_transactions = futures::stream::iter(extrinsics.find::<EthTransact>())
            .then(|extrinsic| async move {
                let extrinsic = extrinsic.context("Failed to decode the eth_transact extrinsic")?;
                let extrinsic_index = extrinsic.details.index() as _;
                anyhow::Result::<_, anyhow::Error>::Ok(EthTransactionExtrinsic {
                    payload: extrinsic.value.payload,
                    extrinsic_index,
                    events: extrinsic
                        .details
                        .events()
                        .await
                        .context("Failed to get extrinsic events")?,
                })
            })
            .try_collect::<Vec<_>>()
            .await
            .context("Failed to get the eth extrinsics")?;
        let consumed_weight = Self::consumed_block_weight(online_block.as_ref())
            .await
            .context("Failed to resolve consumed block weight")?;

        Ok(SubstrateBlockInformation {
            runtime_block: RuntimeSubxtBlock {
                header,
                extrinsics: runtime_extrinsics,
            },
            block_hash: online_block.hash().0,
            consumed_weight,
            limits: limits.context("No substrate block limits available")?,
            online_block,
            eth_transactions,
        })
    }

    fn substrate_block_limits(
        provider: &SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> Result<Weight> {
        let limits = provider
            .constants()
            .at(&revive_metadata::constants().system().block_weights())
            .context("Failed to get the substrate block weight constants")?
            .per_class
            .normal
            .max_extrinsic
            .context("No max extrinsic weight found for normal substrate extrinsics")?;

        Ok(*limits)
    }

    async fn consumed_block_weight(block: &OnlineSubxtBlock) -> Result<Weight> {
        let used = block
            .storage()
            .fetch_or_default(&revive_metadata::storage().system().block_weight())
            .await
            .context("Failed to fetch consumed substrate block weight")?;

        Ok((*used.normal)
            .saturating_add(*used.operational)
            .saturating_add(*used.mandatory))
    }

    fn start_transaction_indexer(
        mut rx: BroadcastReceiver<BlockPair>,
        map: AsyncHashMap<TxHash, IndexedTransactionInformation>,
    ) {
        spawn(async move {
            let result = async move {
                while let Ok(block) = rx.recv().await {
                    match block.substrate_block.as_ref() {
                        Some(substrate_block_information) => {
                            let mut staged_transactions =
                                HashMap::<TxHash, (Option<usize>, Option<usize>)>::new();

                            for (transaction_index, tx_hash) in block
                                .evm_block
                                .transactions
                                .clone()
                                .into_hashes_vec()
                                .into_iter()
                                .enumerate()
                            {
                                staged_transactions
                                    .entry(tx_hash)
                                    .or_default()
                                    .0 = Some(transaction_index);
                            }

                            for extrinsic in substrate_block_information.eth_transactions.iter() {
                                let transaction_hash = keccak256(&extrinsic.payload);
                                staged_transactions
                                    .entry(transaction_hash)
                                    .or_default()
                                    .1 = Some(extrinsic.extrinsic_index);
                            }

                            let indexed_transactions = staged_transactions
                                .into_iter()
                                .map(|(tx_hash, (transaction_index, extrinsic_index))| {
                                    match (transaction_index, extrinsic_index) {
                                        (Some(transaction_index), Some(extrinsic_index)) => Ok((
                                            tx_hash,
                                            IndexedTransactionInformation {
                                                transaction_index,
                                                extrinsic_index: Some(extrinsic_index),
                                                block_pair: block.clone(),
                                            },
                                        )),
                                        (Some(_), None) | (None, Some(_)) => {
                                            warn!(
                                                %tx_hash,
                                                "Tx has an index but not ext index or ext index with no index"
                                            );
                                            Err(anyhow!(
                                                "Tx {tx_hash} has an index but not ext index or ext \
                                                 index with no index"
                                            ))
                                        }
                                        (None, None) => {
                                            unreachable!()
                                        }
                                    }
                                })
                                .collect::<Result<Vec<_>>>()?;
                            map.insert_batch(indexed_transactions).await;
                        }
                        None => {
                            let indexed_transactions = block
                                .evm_block
                                .transactions
                                .clone()
                                .into_hashes_vec()
                                .into_iter()
                                .enumerate()
                                .map(move |(index, hash)| {
                                    (
                                        hash,
                                        IndexedTransactionInformation {
                                            transaction_index: index,
                                            extrinsic_index: None,
                                            block_pair: block.clone(),
                                        },
                                    )
                                });
                            map.insert_batch(indexed_transactions).await;
                        }
                    }
                }
                Result::<(), Error>::Err(anyhow!("Subscription ended prematurely"))
            }
            .await;

            if let Err(err) = result {
                error!(?err, "Transaction indexer task failed");
            }
        });
    }

    fn start_latest_finalized_block_updater(
        field: Arc<RwLock<Option<BlockPair>>>,
        mut rx: BroadcastReceiver<BlockPair>,
    ) {
        spawn(async move {
            while let Ok(latest_block) = rx.recv().await {
                *field.write().await = Some(latest_block)
            }
        });
    }

    fn start_block_by_number_indexer(
        map: AsyncHashMap<u64, BlockPair>,
        mut rx: BroadcastReceiver<BlockPair>,
    ) {
        spawn(async move {
            while let Ok(latest_block) = rx.recv().await {
                let block_number = latest_block.evm_block.number();
                map.insert(block_number, latest_block).await;
            }
        });
    }

    fn start_receipt_provider(
        mut rx: BroadcastReceiver<BlockPair>,
        provider: SingleOrPool<AlloyProvider>,
        substrate_provider: Option<SubstrateProviders>,
        tx_hash_to_receipt_mapping: AsyncHashMap<TxHash, Arc<TransactionReceipt>>,
        block_hash_to_receipt_mapping: AsyncHashMap<BlockHash, Vec<Arc<TransactionReceipt>>>,
    ) {
        let task = async move {
            while let Ok(block) = rx.recv().await {
                let block_hash = block.evm_block.hash();
                let block_receipts = match substrate_provider {
                    Some(ref provider) => Self::construct_block_receipts(block, provider.clone())
                        .await
                        .context("Failed to get substrate block receipts")?,
                    None => provider
                        .get_block_receipts(block.evm_block.hash().into())
                        .await
                        .context("Failed to get the block receipts")?
                        .context("Failed to get the block receipts")?,
                };
                let block_receipts = block_receipts.into_iter().map(Arc::new).collect::<Vec<_>>();
                block_hash_to_receipt_mapping
                    .insert(block_hash, block_receipts.clone())
                    .await;
                tx_hash_to_receipt_mapping
                    .insert_batch(
                        block_receipts
                            .into_iter()
                            .map(|receipt| (receipt.transaction_hash, receipt)),
                    )
                    .await;
            }
            anyhow::Result::<(), anyhow::Error>::Ok(())
        };
        spawn(task);
    }

    fn construct_block_receipts(
        block: BlockPair,
        provider: SubstrateProviders,
    ) -> StaticFuture<Result<Vec<TransactionReceipt>>> {
        Box::pin(async move {
            let substrate_block = block
                .substrate_block
                .as_ref()
                .expect("qed; must be a substrate block")
                .clone();

            let payload = revive_metadata::apis()
                .revive_api()
                .eth_receipt_data()
                .unvalidated();
            let receipt_gas_data = provider
                .runtime_api()
                .at(substrate_block.online_block.hash())
                .call(payload)
                .await
                .context("Failed to get the receipt information")?;

            if receipt_gas_data.len() != substrate_block.eth_transactions.len() {
                bail!(
                    "Receipt data length does not match the number of ethereum transactions in the \
                    block"
                );
            }

            let mut receipts = Vec::with_capacity(receipt_gas_data.len());
            let mut cumulative_gas_used = 0u64;
            let mut log_count = 0usize;
            for (tx_index, (extrinsic, receipt_gas_info)) in substrate_block
                .eth_transactions
                .iter()
                .zip(receipt_gas_data.iter())
                .enumerate()
            {
                let receipt = Self::construct_single_receipt(
                    &block,
                    extrinsic,
                    receipt_gas_info,
                    log_count,
                    cumulative_gas_used,
                    tx_index,
                )
                .context("Failed to construct the receipt")?;
                log_count += receipt.logs().len();
                cumulative_gas_used += receipt.gas_used;
                receipts.push(receipt)
            }
            Ok(receipts)
        })
    }

    fn construct_single_receipt(
        block: &BlockPair,
        extrinsic: &EthTransactionExtrinsic,
        receipt_gas_info: &pallet_revive::ReceiptGasInfo,
        log_count: usize,
        cumulative_gas_used: u64,
        index: usize,
    ) -> Result<TransactionReceipt> {
        let tx_hash = keccak256(&extrinsic.payload);
        let logs = extrinsic
            .events
            .find::<revive_metadata::revive::events::ContractEmitted>()
            .map(|event| -> anyhow::Result<_, anyhow::Error> {
                let event = event.context("Failed to decode the contract-emitted event")?;
                Ok(alloy::primitives::Log {
                    address: event.contract.0.0.into(),
                    data: alloy::primitives::LogData::new_unchecked(
                        event
                            .topics
                            .into_iter()
                            .map(|topic| topic.0.0.into())
                            .collect(),
                        event.data.into(),
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()
            .context("Failed to get the logs")?;
        let logs = alloy::rpc::types::Log::collect_for_receipt(
            log_count,
            alloy::consensus::transaction::TransactionMeta {
                tx_hash,
                index: index as _,
                block_hash: block.evm_block.hash(),
                block_number: block.evm_block.number(),
                base_fee: block.evm_block.header.base_fee_per_gas(),
                excess_blob_gas: block.evm_block.header.excess_blob_gas(),
                timestamp: block.evm_block.header.timestamp,
            },
            logs,
        );
        let is_success = extrinsic
            .events
            .find::<revive_metadata::revive::events::EthExtrinsicRevert>()
            .next()
            .transpose()
            .context("Failed to decode ethereum extrinsic revert event")?
            .is_none();

        let envelope = alloy::consensus::TxEnvelope::decode_2718(&mut extrinsic.payload.as_slice())
            .context("Failed to decode the tx payload")?;
        let transaction_type = envelope.tx_type();
        let sender = envelope
            .recover_signer()
            .context("Failed to recover the address of the signer")?;
        let (receiver, sender_nonce) = match envelope {
            alloy::consensus::EthereumTxEnvelope::Legacy(tx) => (tx.tx().to, tx.tx().nonce),
            alloy::consensus::EthereumTxEnvelope::Eip2930(tx) => (tx.tx().to, tx.tx().nonce),
            alloy::consensus::EthereumTxEnvelope::Eip1559(tx) => (tx.tx().to, tx.tx().nonce),
            alloy::consensus::EthereumTxEnvelope::Eip4844(tx) => match tx.tx() {
                TxEip4844Variant::TxEip4844(tx) => {
                    (alloy::primitives::TxKind::Call(tx.to), tx.nonce)
                }
                TxEip4844Variant::TxEip4844WithSidecar(tx) => {
                    (alloy::primitives::TxKind::Call(tx.tx.to), tx.tx.nonce)
                }
            },
            alloy::consensus::EthereumTxEnvelope::Eip7702(tx) => {
                (alloy::primitives::TxKind::Call(tx.tx().to), tx.tx().nonce)
            }
        };
        let created_contract_address = match receiver {
            alloy::primitives::TxKind::Create => Some(sender.create(sender_nonce)),
            alloy::primitives::TxKind::Call(..) => None,
        };

        let gas_used = u64::try_from(receipt_gas_info.gas_used)
            .map_err(|err| anyhow!("Failed to convert number into u64: {err}"))?;
        let cumulative_gas_used = cumulative_gas_used.saturating_add(gas_used);

        Ok(TransactionReceipt {
            inner: ReceiptEnvelope::from_typed(
                transaction_type,
                Receipt {
                    status: alloy::consensus::Eip658Value::Eip658(is_success),
                    cumulative_gas_used,
                    logs,
                },
            ),
            transaction_hash: tx_hash,
            transaction_index: Some(index as _),
            block_hash: block.evm_block.hash().into(),
            block_number: block.evm_block.number().into(),
            gas_used,
            effective_gas_price: receipt_gas_info
                .effective_gas_price
                .try_into()
                .map_err(|err| anyhow!("Failed to convert number: {err}"))?,
            blob_gas_used: None,
            blob_gas_price: None,
            from: sender,
            to: receiver.into(),
            contract_address: created_contract_address,
        })
    }

    async fn start_eth_rpc_latest_block_number(
        provider: SingleOrPool<AlloyProvider>,
    ) -> Result<WatchSender<u64>> {
        let initial_block_number = provider
            .get_block_number()
            .await
            .context("Failed to get the initial block number from the eth-rpc")?;
        let (tx, _) = watch_channel(initial_block_number);
        tokio::spawn({
            let tx = tx.clone();
            async move {
                let mut interval = interval(Duration::from_millis(500));
                interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    let Ok(block_number) = provider.get_block_number().await else {
                        warn!("Failed to get block number from the eth-rpc");
                        continue;
                    };

                    tx.send_if_modified(|last_observed| {
                        let changed = *last_observed != block_number;
                        *last_observed = block_number;
                        changed
                    });
                }
            }
        });
        Ok(tx)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AvailableEstimationMethods {
    estimate_gas: bool,
    eth_transact_with_config: bool,
    eth_transact: bool,
}

impl AvailableEstimationMethods {
    fn from_provider_pool(provider_pool: &SingleOrPool<OnlineClient<PolkadotConfig>>) -> Self {
        let metadata = provider_pool.metadata();
        let Some(revive_api) = metadata.runtime_api_trait_by_name("ReviveApi") else {
            return Self::default();
        };

        Self {
            estimate_gas: revive_api.method_by_name("eth_estimate_gas").is_some(),
            eth_transact_with_config: revive_api
                .method_by_name("eth_transact_with_config")
                .is_some(),
            eth_transact: revive_api.method_by_name("eth_transact").is_some(),
        }
    }

    fn has_any(self) -> bool {
        self.estimate_gas || self.eth_transact_with_config || self.eth_transact
    }
}

#[derive(Clone, Debug)]
struct SubstrateProviders {
    provider_pool: SingleOrPool<OnlineClient<PolkadotConfig>>,
    available_estimation_methods: AvailableEstimationMethods,
}

impl SubstrateProviders {
    fn new(provider_pool: SingleOrPool<OnlineClient<PolkadotConfig>>) -> Self {
        let available_estimation_methods =
            AvailableEstimationMethods::from_provider_pool(&provider_pool);
        Self {
            provider_pool,
            available_estimation_methods,
        }
    }

    fn available_estimation_methods(&self) -> AvailableEstimationMethods {
        self.available_estimation_methods
    }
}

impl Deref for SubstrateProviders {
    type Target = SingleOrPool<OnlineClient<PolkadotConfig>>;

    fn deref(&self) -> &Self::Target {
        &self.provider_pool
    }
}

impl std::ops::DerefMut for SubstrateProviders {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.provider_pool
    }
}

pub fn new_alloy_provider(
    url: impl ToString,
    config: NodeConnectorConfiguration,
    wallet: Arc<EthereumWallet>,
    nonce_filler: DelayedNonceFiller,
) -> StaticFuture<Result<AlloyProvider>> {
    let url = url.to_string();

    Box::pin(async move {
        let connection = url
            .parse::<BuiltInConnectionString>()
            .context("Failed to parse the RPC connection string")?;
        let is_local = connection.is_local();
        let transport = connection
            .get_transport()
            .await
            .context("Failed to construct the RPC transport")?;

        let client = ClientBuilder::default()
            .layer(ConfiguredTransportLayers { config })
            .transport(transport, is_local);

        Ok(ProviderBuilder::new()
            .disable_recommended_fillers()
            .network::<Ethereum>()
            .filler(GasFiller)
            .fetch_chain_id()
            .filler(nonce_filler)
            .wallet(wallet)
            .connect_client(client))
    })
}

struct ConfiguredTransportLayers {
    config: NodeConnectorConfiguration,
}

impl Layer<BoxTransport> for ConfiguredTransportLayers {
    type Service = BoxTransport;

    fn layer(&self, transport: BoxTransport) -> Self::Service {
        let transport = handle_concurrency_config(transport, &self.config);
        let transport = handle_global_concurrency_config(transport, &self.config);
        let transport = handle_batching_config(transport, &self.config);
        handle_retry_config(transport, &self.config)
    }
}

fn handle_concurrency_config(
    transport: BoxTransport,
    config: &NodeConnectorConfiguration,
) -> BoxTransport {
    let config = config
        .eth_provider_configuration
        .as_ref()
        .and_then(|config| config.concurrency_configuration.as_ref());
    match config {
        Some(ProviderConcurrencyConfiguration::Disabled) | None => transport,
        Some(ProviderConcurrencyConfiguration::SemaphoreBasedLimiter { permits }) => {
            ConcurrencyLimiterLayer::new(*permits)
                .layer(transport)
                .as_boxed()
        }
    }
}

fn handle_global_concurrency_config(
    transport: BoxTransport,
    config: &NodeConnectorConfiguration,
) -> BoxTransport {
    static GLOBAL_CONCURRENCY_LAYERS: LazyLock<StdMutex<HashMap<usize, ConcurrencyLimiterLayer>>> =
        LazyLock::new(Default::default);

    let config = config
        .eth_provider_configuration
        .as_ref()
        .and_then(|config| config.global_concurrency_configuration.as_ref());
    let permits = match config {
        Some(ProviderConcurrencyConfiguration::Disabled) => return transport,
        Some(ProviderConcurrencyConfiguration::SemaphoreBasedLimiter { permits }) => *permits,
        None => 1_000,
    };
    let layer = GLOBAL_CONCURRENCY_LAYERS
        .lock()
        .expect("poisoned")
        .entry(permits)
        .or_insert_with(|| ConcurrencyLimiterLayer::new(permits))
        .clone();
    layer.layer(transport).boxed()
}

fn handle_batching_config(
    transport: BoxTransport,
    config: &NodeConnectorConfiguration,
) -> BoxTransport {
    let config = config
        .eth_provider_configuration
        .as_ref()
        .and_then(|config| config.batching_configuration.as_ref());
    match config {
        Some(BatchingConfiguration::Disabled) | None => transport,
        Some(BatchingConfiguration::Enabled {
            max_batch_size,
            batching_duration,
        }) => BatchingLayer::new()
            .with_max_batch_size(*max_batch_size)
            .with_batching_duration(*batching_duration)
            .layer(transport)
            .as_boxed(),
    }
}

fn handle_retry_config(
    transport: BoxTransport,
    config: &NodeConnectorConfiguration,
) -> BoxTransport {
    let config = config
        .eth_provider_configuration
        .as_ref()
        .and_then(|config| config.retry_configuration.as_ref());
    match config {
        Some(RetryConfiguration::Enabled {
            polling_duration,
            initial_backoff,
            max_backoff,
        }) => RetryLayer::default()
            .with_polling_duration(*polling_duration)
            .with_initial_backoff(*initial_backoff)
            .with_max_backoff(*max_backoff)
            .layer(transport)
            .as_boxed(),
        None => RetryLayer::default().layer(transport).as_boxed(),
        Some(RetryConfiguration::Disabled) => transport,
    }
}

#[derive(Clone)]
pub struct BlockPair {
    pub observation_time: SystemTime,
    pub evm_block: Arc<EvmBlock>,
    pub substrate_block: Option<Arc<SubstrateBlockInformation>>,
}

pub struct SubstrateBlockInformation {
    pub runtime_block: RuntimeSubxtBlock,
    pub block_hash: [u8; 32],
    pub consumed_weight: Weight,
    pub limits: Weight,
    pub eth_transactions: Vec<EthTransactionExtrinsic>,
    online_block: Arc<OnlineSubxtBlock>,
}

#[derive(Clone)]
struct UnresolvedBlockPair {
    observation_time: SystemTime,
    evm_block: Arc<EvmBlock>,
    substrate_block: Option<Arc<OnlineSubxtBlock>>,
}

pub struct EthTransactionExtrinsic {
    pub payload: Vec<u8>,
    pub extrinsic_index: usize,
    pub events: subxt::blocks::ExtrinsicEvents<PolkadotConfig>,
}

#[derive(Clone)]
pub struct IndexedTransactionInformation {
    pub transaction_index: usize,
    pub extrinsic_index: Option<usize>,
    pub block_pair: BlockPair,
}

fn geth_trace_options_to_revive_tracer_type(
    trace_options: GethDebugTracingOptions,
) -> Result<TracerType> {
    let trace_options = serde_json::to_value(trace_options)
        .expect("qed; alloy geth tracing options serialize to JSON");
    let trace_options = serde_json::from_value::<TracerConfig>(trace_options)
        .context("Failed to convert geth tracing options into revive tracer config")?;
    Ok(trace_options.config)
}

fn transaction_request_to_revive_transaction(
    tx: TransactionRequest,
) -> Result<pallet_revive::evm::GenericTransaction> {
    let tx_json =
        serde_json::to_value(tx).expect("qed; alloy transaction request serializes to JSON");
    serde_json::from_value::<pallet_revive::evm::GenericTransaction>(tx_json)
        .context("Failed to convert alloy transaction request into revive transaction")
}

fn revive_trace_to_geth_trace(trace: pallet_revive::evm::Trace) -> Result<GethTrace> {
    let trace_json =
        serde_json::to_value(trace).expect("qed; pallet-revive trace serializes to JSON");
    serde_json::from_value::<GethTrace>(trace_json)
        .context("Failed to deserialize revive trace into geth trace")
}
