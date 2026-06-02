use crate::internal_prelude::*;

type AlloyProvider = FillProvider<
    JoinFill<
        JoinFill<JoinFill<JoinFill<Identity, FallbackGasFiller>, ChainIdFiller>, NonceFiller>,
        WalletFiller<Arc<EthereumWallet>>,
    >,
    RootProvider,
>;

type RuntimeHeader = sp_runtime::generic::Header<u32, sp_runtime::traits::BlakeTwo256>;
type RuntimeBlock = sp_runtime::generic::Block<RuntimeHeader, sp_runtime::OpaqueExtrinsic>;

pub struct NodeConnector {
    eth_providers: SingleOrPool<AlloyProvider>,
    substrate_providers: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
    /* State */
    latest_finalized_block: Arc<RwLock<Option<Arc<BlockPair>>>>,
    finalized_block_broadcast: BroadcastSender<Arc<BlockPair>>,
    indexed_transactions: AsyncHashMap<TxHash, IndexedTransactionInformation>,
    /* Auxiliary */
    _node: Box<dyn NodeConfiguration>,
}

impl NodeConnector {
    pub fn new<N: NodeConfiguration + Send + 'static>(
        node: N,
        wallet: Arc<EthereumWallet>,
        config: NodeConnectorConfiguration,
    ) -> StaticFuture<Result<Self>> {
        let eth_provider_urls = node.eth_provider_url();
        let substrate_rpc_urls = node.substrate_provider_url();

        let eth_providers_future = Self::eth_providers_future(eth_provider_urls, config, wallet);
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

            let latest_finalized_block = Arc::new(RwLock::new(None));
            let finalized_blocks_broadcast_tx = Self::start_finalized_blocks_broadcaster(
                eth_providers.clone(),
                substrate_providers.clone(),
            );
            let indexed_transactions = AsyncHashMap::new();
            Self::start_transaction_indexer(
                finalized_blocks_broadcast_tx.subscribe(),
                indexed_transactions.clone(),
            );
            Self::start_latest_finalized_block_updater(
                latest_finalized_block.clone(),
                finalized_blocks_broadcast_tx.subscribe(),
            );

            Ok(Self {
                eth_providers,
                substrate_providers,
                latest_finalized_block,
                finalized_block_broadcast: finalized_blocks_broadcast_tx,
                indexed_transactions,
                _node: Box::new(node),
            })
        })
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

    pub fn send_transaction(&self, tx: TransactionRequest) -> StaticFuture<Result<TxHash>> {
        match self.substrate_providers.as_ref() {
            Some(substrate_providers) => {
                self.send_transaction_substrate(tx, substrate_providers.clone())
            }
            None => self.send_transaction_evm(tx),
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
        Box::pin(async move {
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
                .context("Failed to create an unsigned transaction")?
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
                    Result::<(), anyhow::Error>::Ok(())
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

    pub fn subscribe_to_finalized_blocks(&self) -> StaticStream<Arc<BlockPair>> {
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
            let substrate_block = indexed_transaction
                .block_pair
                .substrate_block
                .as_ref()
                .expect("qed; this is a substrate transaction");
            let extrinsic_index = indexed_transaction
                .extrinsic_index
                .expect("qed; this is a substrate transaction");
            let extrinsic_index = extrinsic_index as u32;

            let parent_hash = substrate_block.header().parent_hash;
            let header = RuntimeHeader::decode(&mut substrate_block.header().encode().as_slice())
                .context(
                "Failed to decode the substrate block header into the runtime header type",
            )?;
            let extrinsics = substrate_block
                .extrinsics()
                .await
                .context("Failed to get the substrate block extrinsics")?
                .iter()
                .map(|extrinsic| {
                    sp_runtime::OpaqueExtrinsic::from_bytes(extrinsic.bytes())
                        .context("Failed to decode the extrinsic into an opaque extrinsic")
                })
                .collect::<Result<Vec<_>>>()?;
            let block = RuntimeBlock { header, extrinsics };

            let trace_options = serde_json::to_value(trace_options)
                .expect("qed; alloy geth tracing options serialize to JSON");
            let trace_options =
                serde_json::from_value::<pallet_revive::evm::TracerConfig>(trace_options)
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
            let payload = revive_metadata::apis()
                .revive_api()
                .balance(address.0.0.into())
                .unvalidated();
            let runtime_api = match latest_block.read().await.as_ref() {
                Some(latest_block) => provider.runtime_api().at(latest_block
                    .substrate_block
                    .as_ref()
                    .expect("qed; this is a substrate node")
                    .hash()),
                None => provider
                    .runtime_api()
                    .at_latest()
                    .await
                    .context("Failed to get the runtime API at the latest finalized block")?,
            };
            let balance = runtime_api
                .call(payload)
                .await
                .context("Failed to get the balance")?;
            Ok(U256::from_limbs_slice(&balance.0))
        })
    }

    fn eth_providers_future(
        eth_provider_urls: NodeUrlCollection<'_>,
        config: NodeConnectorConfiguration,
        wallet: Arc<EthereumWallet>,
    ) -> StaticFuture<Result<SingleOrPool<AlloyProvider>>> {
        match eth_provider_urls {
            NodeUrlCollection { ipc: Some(url), .. }
            | NodeUrlCollection {
                ipc: None,
                ws: None,
                http: Some(url),
            } => new_alloy_provider(url, config, wallet.clone())
                .map_ok(SingleOrPool::Single)
                .boxed() as StaticFuture<_>,
            NodeUrlCollection {
                ipc: None,
                ws: Some(url),
                ..
            } => try_join_all(
                (0..10).map(move |_| new_alloy_provider(url.as_ref(), config, wallet.clone())),
            )
            .map_ok(Pool::new_unchecked)
            .map_ok(SingleOrPool::Pool)
            .boxed() as StaticFuture<_>,
            NodeUrlCollection {
                ipc: None,
                ws: None,
                http: None,
            } => Box::pin(ready(Err(anyhow::anyhow!(
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
                    OnlineClient::<PolkadotConfig>::from_url(url)
                        .map_ok(SingleOrPool::Single)
                        .map_err(anyhow::Error::from)
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
                    try_join_all(
                        (0..10).map(move |_| OnlineClient::<PolkadotConfig>::from_url(url.clone())),
                    )
                    .map_err(anyhow::Error::from)
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

    fn start_finalized_blocks_broadcaster(
        provider: SingleOrPool<AlloyProvider>,
        substrate_provider: Option<SingleOrPool<OnlineClient<PolkadotConfig>>>,
    ) -> BroadcastSender<Arc<BlockPair>> {
        let (tx, _) = broadcast_channel::<Arc<BlockPair>>(2048);
        let task = match substrate_provider {
            Some(provider) => {
                Self::start_finalized_blocks_broadcaster_substrate(provider, tx.clone())
            }
            None => Self::start_finalized_blocks_broadcaster_evm(provider, tx.clone()),
        };
        tokio::spawn(task);
        tx
    }

    fn start_finalized_blocks_broadcaster_evm(
        provider: SingleOrPool<AlloyProvider>,
        tx: BroadcastSender<Arc<BlockPair>>,
    ) -> StaticFuture<Result<()>> {
        Box::pin(async move {
            let mut subscription = provider
                .subscribe_full_blocks()
                .hashes()
                .into_stream()
                .await?;
            while let Some(Ok(block)) = subscription.next().await {
                let block_pair = Arc::new(BlockPair {
                    observation_time: SystemTime::now(),
                    evm_block: block,
                    substrate_block: None,
                });
                let _ = tx.send(block_pair);
            }
            bail!("Block subscription ended prematurely")
        })
    }

    fn start_finalized_blocks_broadcaster_substrate(
        provider: SingleOrPool<OnlineClient<PolkadotConfig>>,
        tx: BroadcastSender<Arc<BlockPair>>,
    ) -> StaticFuture<Result<()>> {
        Box::pin(async move {
            let mut subscription = provider.blocks().subscribe_finalized().await?;
            while let Some(Ok(substrate_block)) = subscription.next().await {
                let runtime_api = substrate_block
                    .runtime_api()
                    .await
                    .context("Failed to get the runtime API")?;

                let eth_block =
                    retry_with_exponential_backoff(10, Duration::from_millis(10), || {
                        let runtime_api = &runtime_api;
                        let payload = revive_metadata::apis()
                            .revive_api()
                            .eth_block()
                            .unvalidated();

                        async move {
                            match runtime_api.call(payload).await {
                                Ok(block) => ControlFlow::Break(block),
                                Err(err) => ControlFlow::Continue(err),
                            }
                        }
                    })
                    .await
                    .context("Failed to call the eth_block runtime API")?;
                let eth_block_json = serde_json::to_value(eth_block.0)
                    .expect("qed; pallet-revive eth block serializes to JSON");
                let evm_block = serde_json::from_value::<alloy::rpc::types::Block>(eth_block_json)
                    .expect("qed; pallet-revive and alloy block JSON formats are identical");

                let block_pair = Arc::new(BlockPair {
                    observation_time: SystemTime::now(),
                    evm_block,
                    substrate_block: Some(substrate_block),
                });
                let _ = tx.send(block_pair);
            }
            bail!("Block subscription ended prematurely")
        })
    }

    fn start_transaction_indexer(
        mut rx: BroadcastReceiver<Arc<BlockPair>>,
        map: AsyncHashMap<TxHash, IndexedTransactionInformation>,
    ) {
        tokio::spawn(async move {
            let result = async move {
                while let Ok(block) = rx.recv().await {
                    match block.substrate_block.as_ref() {
                        Some(substrate_block) => {
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

                            let extrinsics = substrate_block
                                .extrinsics()
                                .await
                                .context("Failed to get the substrate block extrinsics")?;
                            for extrinsic in extrinsics.find::<EthTransact>() {
                                let extrinsic = extrinsic
                                    .context("Failed to decode the eth_transact extrinsic")?;
                                let transaction_hash = keccak256(&extrinsic.value.payload);
                                let extrinsic_index = usize::try_from(extrinsic.details.index())
                                    .expect("qed; subxt extrinsic indices must fit in usize");

                                staged_transactions
                                    .entry(transaction_hash)
                                    .or_default()
                                    .1 = Some(extrinsic_index);
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
                anyhow::Result::<(), anyhow::Error>::Err(anyhow!("Subscription ended prematurely"))
            }
            .await;

            if let Err(err) = result {
                error!(?err, "Transaction indexer task failed");
            }
        });
    }

    fn start_latest_finalized_block_updater(
        field: Arc<RwLock<Option<Arc<BlockPair>>>>,
        mut rx: BroadcastReceiver<Arc<BlockPair>>,
    ) {
        tokio::spawn(async move {
            while let Ok(latest_block) = rx.recv().await {
                *field.write().await = Some(latest_block)
            }
        });
    }
}

pub fn new_alloy_provider(
    url: impl ToString,
    config: NodeConnectorConfiguration,
    wallet: Arc<EthereumWallet>,
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
            .with_cached_nonce_management()
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
        Some(ConcurrencyConfiguration::Disabled) | None => transport,
        Some(ConcurrencyConfiguration::SemaphoreBasedLimiter { permits }) => {
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
        Some(ConcurrencyConfiguration::Disabled) => return transport,
        Some(ConcurrencyConfiguration::SemaphoreBasedLimiter { permits }) => *permits,
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
        }) => RetryLayer::new(*polling_duration, *initial_backoff, *max_backoff)
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

pub struct BlockPair {
    pub observation_time: SystemTime,
    pub evm_block: alloy::rpc::types::Block,
    pub substrate_block: Option<subxt::blocks::Block<PolkadotConfig, OnlineClient<PolkadotConfig>>>,
}

#[derive(Clone)]
pub struct IndexedTransactionInformation {
    pub transaction_index: usize,
    pub extrinsic_index: Option<usize>,
    pub block_pair: Arc<BlockPair>,
}
