use crate::internal_prelude::*;

type AlloyProvider = FillProvider<
    JoinFill<
        JoinFill<JoinFill<JoinFill<Identity, FallbackGasFiller>, ChainIdFiller>, NonceFiller>,
        WalletFiller<Arc<EthereumWallet>>,
    >,
    RootProvider,
>;

pub type RuntimeSubxtBlock = GenericBlock<GenericHeader<u32, BlakeTwo256>, OpaqueExtrinsic>;
type OnlineSubxtBlock = SubxtBlock<PolkadotConfig, OnlineClient<PolkadotConfig>>;

pub struct NodeConnector {
    eth_providers: SingleOrPool<AlloyProvider>,
    substrate_providers: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
    config: NodeConnectorConfiguration,
    /* State */
    latest_finalized_block: Arc<RwLock<Option<BlockPair>>>,
    finalized_block_broadcast: BroadcastSender<BlockPair>,
    blocks_by_number: AsyncHashMap<u64, BlockPair>,
    indexed_transactions: AsyncHashMap<TxHash, IndexedTransactionInformation>,
    submission_locks: DashMap<Address, Arc<Mutex<()>>>,
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

        let eth_provider_urls = node.eth_provider_url();
        let substrate_rpc_urls = node.substrate_provider_url();
        let nonce_manager = CachedNonceManager::default();

        let eth_providers_future =
            Self::eth_providers_future(eth_provider_urls, config, wallet, nonce_manager);
        let substrate_providers_future = Self::substrate_providers_future(substrate_rpc_urls);

        Box::pin(async move {
            let eth_providers = eth_providers_future
                .await
                .context("Failed to get the eth providers")?;
            let substrate_providers = match substrate_providers_future {
                Some(substrate_providers_future) => Some(
                    substrate_providers_future
                        .await
                        .context("Failed to get the substrate providers")?,
                ),
                None => None,
            };

            let unresolved_finalized_blocks_broadcast_tx =
                Self::start_unresolved_finalized_blocks_broadcaster(
                    eth_providers.clone(),
                    substrate_providers.clone(),
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

            Ok(Self {
                eth_providers,
                substrate_providers,
                config,
                latest_finalized_block,
                finalized_block_broadcast: finalized_blocks_broadcast_tx,
                blocks_by_number,
                indexed_transactions,
                submission_locks: DashMap::new(),
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
    ) -> StaticFuture<Result<TransactionReceipt>> {
        let provider = self.eth_providers.clone();
        let substrate_provider = self.substrate_providers.clone();
        let indexed_transactions = self.indexed_transactions.clone();
        self.send_transaction(tx)
            .and_then(move |tx_hash| {
                Self::get_receipt_internal(
                    provider,
                    substrate_provider,
                    indexed_transactions,
                    tx_hash,
                )
            })
            .boxed()
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
                self.send_transaction_and_await_validation_substrate(tx, substrate_provider.clone())
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
        Box::pin(async move {
            provider
                .send_transaction(tx)
                .await
                .context("Failed to send transaction through the provider")
                .map(|builder| *builder.tx_hash())
        })
    }

    fn send_transaction_substrate(
        &self,
        tx: TransactionRequest,
        substrate_provider: SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> StaticFuture<Result<TxHash>> {
        let provider = self.eth_providers.clone();
        let submission_mutex = tx
            .from
            .map(|addr| self.submission_locks.entry(addr).or_default().clone());
        Box::pin(async move {
            let _guard = match submission_mutex {
                Some(mutex) => Some(mutex.lock_owned().await),
                None => None,
            };
            let signed_transaction = provider
                .fill(tx)
                .await
                .context("Failed to fill transaction")?
                .try_into_envelope()
                .context("Failed to construct envelope from filled transaction")?;
            let ethereum_tx_hash = signed_transaction.tx_hash();
            let payload = signed_transaction.encoded_2718();

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

            Ok(*ethereum_tx_hash)
        })
    }

    fn send_transaction_and_await_validation_substrate(
        &self,
        tx: TransactionRequest,
        substrate_provider: SingleOrPool<OnlineClient<PolkadotConfig>>,
    ) -> StaticFuture<Result<TxHash>> {
        let provider = self.eth_providers.clone();
        let submission_mutex = tx
            .from
            .map(|addr| self.submission_locks.entry(addr).or_default().clone());

        Box::pin(async move {
            let _guard = match submission_mutex {
                Some(mutex) => Some(mutex.lock_owned().await),
                None => None,
            };

            let signed_transaction = provider
                .fill(tx)
                .await
                .context("Failed to fill transaction")?
                .try_into_envelope()
                .context("Failed to construct envelope from filled transaction")?;
            let ethereum_tx_hash = signed_transaction.tx_hash();
            let payload = signed_transaction.encoded_2718();

            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec())
                .unvalidated();
            let substrate_transaction = substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create substrate transaction")?;
            let substrate_tx_hash = substrate_transaction.hash();

            info!(
                evm_hash = %ethereum_tx_hash,
                substrate_hash = ?substrate_tx_hash,
                "Create a substrate transaction, but didn't yet submit it"
            );

            let mut watch = substrate_transaction
                .submit_and_watch()
                .await
                .context("Failed to submit transaction")?;
            loop {
                match watch.next().await {
                    Some(Ok(subxt::tx::TxStatus::InFinalizedBlock(..))) => break,
                    Some(Ok(..)) => {}
                    Some(Err(err)) => Err(err).context("Failed to get the transaction status")?,
                    None => bail!("Transaction status closed before we could receive it"),
                }
            }

            Ok(*ethereum_tx_hash)
        })
    }

    // TODO: Do we want the connector to cache the receipts in memory too as the blocks become
    // available in order to eliminate any issues with the eth-rpc?
    pub fn get_receipt(&self, tx_hash: TxHash) -> StaticFuture<Result<TransactionReceipt>> {
        Self::get_receipt_internal(
            self.eth_providers.clone(),
            self.substrate_providers.clone(),
            self.indexed_transactions.clone(),
            tx_hash,
        )
    }

    fn get_receipt_internal(
        provider: SingleOrPool<AlloyProvider>,
        substrate_provider: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
        indexed_transactions: AsyncHashMap<TxHash, IndexedTransactionInformation>,
        tx_hash: TxHash,
    ) -> StaticFuture<Result<TransactionReceipt>> {
        Box::pin(async move {
            let block = indexed_transactions.get(tx_hash).await;

            if substrate_provider.is_some() {
                timeout(Duration::from_secs(5 * 60), async {
                    while provider
                        .get_block_number()
                        .await
                        .context("Failed to get block number")?
                        < block.block_pair.evm_block.number()
                    {
                        sleep(Duration::from_millis(200)).await
                    }
                    Result::<(), Error>::Ok(())
                })
                .await
                .context("Failed to wait for eth-rpc block to advance")?
                .context("Failed to wait for eth-rpc block to advance")?
            }

            provider
                .get_transaction_receipt(tx_hash)
                .await
                .context("Failed to get transaction receipt")?
                .context("Failed to get transaction receipt")
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

    // TODO: Add a different path to take when we want to do this through subxt since we know that
    // it's supported there.
    pub fn estimate_gas(&self, tx: TransactionRequest) -> StaticFuture<Result<u64>> {
        self.eth_providers
            .clone()
            .estimate_gas(tx)
            .into_future()
            .map(|res| res.context("Failed to get the gas estimate"))
            .boxed()
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

            let trace_options = serde_json::to_value(trace_options)
                .expect("qed; alloy geth tracing options serialize to JSON");
            let trace_options = serde_json::from_value::<TracerConfig>(trace_options)
                .context("Failed to convert geth tracing options into revive tracer config")?;
            let payload = revive_metadata::apis()
                .revive_api()
                .trace_tx(block.into(), extrinsic_index, trace_options.config.into())
                .unvalidated();
            let trace = provider
                .runtime_api()
                .at(parent_hash)
                .call(payload)
                .await
                .context("Failed to get the transaction trace")?
                .context("Failed to get the transaction trace")?
                .0;

            let trace_json =
                serde_json::to_value(trace).expect("qed; pallet-revive trace serializes to JSON");
            serde_json::from_value::<GethTrace>(trace_json)
                .context("Failed to deserialize revive trace into geth trace")
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
                .balance(address.0.0.into())
                .unvalidated();
            let balance = runtime_api
                .call(payload)
                .await
                .context("Failed to get the balance")?;
            Ok(U256::from_limbs_slice(&balance.0))
        })
    }

    pub fn block(&self, number: u64) -> StaticFuture<BlockPair> {
        self.blocks_by_number.get(number)
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
        nonce_manager: CachedNonceManager,
    ) -> StaticFuture<Result<SingleOrPool<AlloyProvider>>> {
        match eth_provider_urls {
            NodeUrlCollection { ipc: Some(url), .. }
            | NodeUrlCollection {
                ipc: None,
                ws: None,
                http: Some(url),
            } => new_alloy_provider(url, config, wallet.clone(), nonce_manager)
                .map_ok(SingleOrPool::Single)
                .boxed() as StaticFuture<_>,
            NodeUrlCollection {
                ipc: None,
                ws: Some(url),
                ..
            } => try_join_all((0..10).map(move |_| {
                new_alloy_provider(url.as_ref(), config, wallet.clone(), nonce_manager.clone())
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

    fn substrate_providers_future(
        substrate_provider_urls: Option<NodeUrlCollection<'_>>,
    ) -> Option<StaticFuture<Result<SingleOrPool<OnlineClient<PolkadotConfig>>>>> {
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
                        .map_ok(SingleOrPool::Single)
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
                        .map_ok(Pool::new_unchecked)
                        .map_ok(SingleOrPool::Pool)
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
        substrate_provider: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
    ) -> BroadcastSender<UnresolvedBlockPair> {
        let (tx, _) = broadcast_channel::<UnresolvedBlockPair>(2048);
        let task = match substrate_provider {
            Some(provider) => {
                Self::start_unresolved_finalized_blocks_broadcaster_substrate(provider, tx.clone())
            }
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
            bail!("Block subscription ended prematurely")
        })
    }

    fn start_unresolved_finalized_blocks_broadcaster_substrate(
        provider: SingleOrPool<OnlineClient<PolkadotConfig>>,
        tx: BroadcastSender<UnresolvedBlockPair>,
    ) -> StaticFuture<Result<()>> {
        Box::pin(async move {
            loop {
                let mut subscription = provider
                    .blocks()
                    .subscribe_finalized()
                    .await
                    .context("Failed to subscribe to finalized substrate blocks")?;

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
                    let evm_block = serde_json::from_value::<EvmBlock>(eth_block_json)
                        .expect("qed; pallet-revive and alloy block JSON formats are identical");

                    let block_pair = UnresolvedBlockPair {
                        observation_time: SystemTime::now(),
                        evm_block: Arc::new(evm_block),
                        substrate_block: Some(Arc::new(substrate_block)),
                    };
                    let _ = tx.send(block_pair);
                }

                warn!("Finalized substrate block subscription ended; resubscribing");
            }
        })
    }

    fn start_resolved_finalized_blocks_broadcaster(
        mut rx: BroadcastReceiver<UnresolvedBlockPair>,
        substrate_provider: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
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
        let eth_transactions = extrinsics
            .find::<EthTransact>()
            .map(|extrinsic| {
                let extrinsic = extrinsic.context("Failed to decode the eth_transact extrinsic")?;
                let extrinsic_index = extrinsic.details.index() as _;
                Ok(EthTransactionExtrinsic {
                    payload: extrinsic.value.payload,
                    extrinsic_index,
                })
            })
            .collect::<Result<Vec<_>>>()?;
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
}

pub fn new_alloy_provider(
    url: impl ToString,
    config: NodeConnectorConfiguration,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
) -> StaticFuture<Result<AlloyProvider>> {
    let url = url.to_string();

    Box::pin(async move {
        let connection = url
            .parse::<BuiltInConnectionString>()
            .context("Failed to parse the RPC connection string")?;
        let is_local = connection.is_local();
        let mut transport = connection
            .get_transport()
            .await
            .context("Failed to construct the RPC transport")?;

        transport = handle_concurrency_config(transport, &config);
        transport = handle_global_concurrency_config(transport, &config);
        transport = handle_batching_config(transport, &config);
        transport = handle_retry_config(transport, &config);
        let client = RpcClient::new(transport, is_local);

        Ok(ProviderBuilder::new()
            .disable_recommended_fillers()
            .network::<Ethereum>()
            .filler(handle_gas_filler_config(&config))
            .fetch_chain_id()
            .with_nonce_management(nonce_manager)
            .wallet(wallet)
            .connect_client(client))
    })
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

fn handle_gas_filler_config(config: &NodeConnectorConfiguration) -> FallbackGasFiller {
    let config = config
        .eth_provider_configuration
        .as_ref()
        .and_then(|config| config.gas_filler_configuration.as_ref());
    match config {
        Some(GasFillerConfiguration::DisableTracingFallback) | None => {
            FallbackGasFiller::new().with_fallback_mechanism_disabled()
        }
        Some(GasFillerConfiguration::EnableTracingFallback) => {
            FallbackGasFiller::new().with_fallback_mechanism_enabled()
        }
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
}

#[derive(Clone)]
pub struct IndexedTransactionInformation {
    pub transaction_index: usize,
    pub extrinsic_index: Option<usize>,
    pub block_pair: BlockPair,
}
