use crate::internal_prelude::*;

#[derive(Clone)]
pub struct TransactionFinder(Arc<State>);

impl TransactionFinder {
    pub fn new(
        provider: DynProvider,
        substrate_provider: Option<OnlineClient<PolkadotConfig>>,
    ) -> Self {
        match substrate_provider {
            Some(substrate_provider) => Self::new_substrate(substrate_provider),
            None => Self::new_ethereum(provider),
        }
    }

    fn new_ethereum(provider: DynProvider) -> Self {
        let transactions_map = AsyncHashMap::<TxHash, (usize, Arc<TransactionFinderBlock>)>::new();
        let cloned_map = transactions_map.clone();
        let subscription_task = tokio::spawn(
            async move {
                let transactions_map = cloned_map;
                let mut subscription = provider
                    .subscribe_full_blocks()
                    .full()
                    .into_stream()
                    .await
                    .context("Failed to subscribe to new blocks through the alloy provider")?;
                loop {
                    let next = subscription.next().await;
                    match next {
                        Some(Ok(block)) => {
                            let block = Arc::new(TransactionFinderBlock {
                                evm_block: block,
                                substrate_block: None,
                            });
                            transactions_map
                                .batch_insert(
                                    block
                                        .evm_block
                                        .transactions
                                        .clone()
                                        .into_hashes_vec()
                                        .into_iter()
                                        .enumerate()
                                        .map(move |(index, hash)| (hash, (index, block.clone()))),
                                )
                                .await;
                        }
                        Some(Err(err)) => {
                            error!(?err, "Failed to fetch the block");
                            Err(err).context("Failed to fetch the block")?
                        }
                        None => {
                            error!("Subscription closed prematurely");
                            bail!("Block subscription ended prematurely")
                        }
                    }
                }
                #[allow(unreachable_code)]
                anyhow::Result::<(), anyhow::Error>::Ok(())
            }
            .instrument(info_span!("Transaction Finder")),
        );

        Self(Arc::new(State {
            map: transactions_map,
            task: subscription_task.abort_handle(),
        }))
    }

    fn new_substrate(provider: OnlineClient<PolkadotConfig>) -> Self {
        let transactions_map = AsyncHashMap::<TxHash, (usize, Arc<TransactionFinderBlock>)>::new();
        let cloned_map = transactions_map.clone();
        let subscription_task = tokio::spawn(
            async move {
                let transactions_map = cloned_map;
                let mut subscription =
                    provider.blocks().subscribe_finalized().await.context(
                        "Failed to subscribe to finalized blocks through subxt provider",
                    )?;
                loop {
                    match subscription.next().await {
                        Some(Ok(substrate_block)) => {
                            let evm_block = {
                                let mut interval =
                                    tokio::time::interval(Duration::from_millis(250));
                                loop {
                                    interval.tick().await;
                                    let result = async {
                                        let encoded_block = provider
                                            .runtime_api()
                                            .at(substrate_block.reference())
                                            .call_raw("ReviveApi_eth_block", None)
                                            .await
                                            .context("Failed to call the eth_block runtime API")?;
                                        let eth_block = EvmBlock::decode(
                                            &mut encoded_block.as_slice(),
                                        )
                                        .context(
                                            "Failed to decode the eth_block runtime API result",
                                        )?;
                                        let serialized = serde_json::to_value(eth_block)
                                            .context("Failed to serialize the runtime API block")?;
                                        serde_json::from_value::<alloy::rpc::types::Block>(
                                            serialized,
                                        )
                                        .context("Failed to deserialize into the alloy block type")
                                    }
                                    .await;
                                    if let Ok(block) = result {
                                        break block;
                                    }
                                }
                            };

                            let extrinsics = substrate_block
                                .extrinsics()
                                .await
                                .context("Failed to get extrinsics")?;
                            let block = Arc::new(TransactionFinderBlock {
                                evm_block,
                                substrate_block: Some(substrate_block),
                            });
                            transactions_map
                                .batch_insert(
                                    extrinsics
                                        .find::<revive_metadata::revive::calls::types::EthTransact>(
                                        )
                                        .flatten()
                                        .map(move |ext| {
                                            let tx_hash =
                                                alloy::primitives::keccak256(&ext.value.payload);
                                            let index = ext.details.index();
                                            (tx_hash, (index as usize, block.clone()))
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                .await;
                        }
                        Some(Err(err)) => {
                            error!(?err, "Failed to fetch the block");
                            Err(err).context("Failed to fetch the block")?
                        }
                        None => {
                            error!("Subscription closed prematurely");
                            bail!("Block subscription ended prematurely")
                        }
                    }
                }
                #[allow(unreachable_code)]
                anyhow::Result::<(), anyhow::Error>::Ok(())
            }
            .instrument(info_span!("Transaction Finder")),
        );

        Self(Arc::new(State {
            map: transactions_map,
            task: subscription_task.abort_handle(),
        }))
    }

    pub fn find(&self, tx_hash: TxHash) -> FrameworkFuture<(usize, Arc<TransactionFinderBlock>)> {
        self.0.map.get(tx_hash)
    }
}

struct State {
    map: AsyncHashMap<TxHash, (usize, Arc<TransactionFinderBlock>)>,
    task: tokio::task::AbortHandle,
}

impl Drop for State {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct TransactionFinderBlock {
    // Unhydrated ethereum block.
    pub evm_block: alloy::rpc::types::Block,
    // Substrate block.
    pub substrate_block: Option<subxt::blocks::Block<PolkadotConfig, OnlineClient<PolkadotConfig>>>,
}
