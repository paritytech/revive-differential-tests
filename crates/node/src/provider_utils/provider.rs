use crate::internal_prelude::*;

pub type ConcreteProvider<N, W> = FillProvider<
    JoinFill<
        JoinFill<
            JoinFill<JoinFill<Identity, FallbackGasFiller>, ChainIdFiller>,
            NonceFiller<ZeroedCachedNonceManager>,
        >,
        WalletFiller<W>,
    >,
    RootProvider<N>,
    N,
>;

pub async fn construct_concurrency_limited_provider<N, W>(
    rpc_url: &str,
    fallback_gas_filler: FallbackGasFiller,
    chain_id_filler: ChainIdFiller,
    nonce_filler: NonceFiller<ZeroedCachedNonceManager>,
    wallet: W,
) -> Result<ConcreteProvider<N, W>>
where
    N: Network<TransactionRequest: TransactionBuilder4844>,
    W: NetworkWallet<N>,
    Identity: TxFiller<N>,
    FallbackGasFiller: TxFiller<N>,
    ChainIdFiller: TxFiller<N>,
    NonceFiller<ZeroedCachedNonceManager>: TxFiller<N>,
    WalletFiller<W>: TxFiller<N>,
{
    // This is a global limit on the RPC concurrency that applies to all of the providers created
    // by the framework. With this limit, it means that we can have a maximum of N concurrent
    // requests at any point of time and no more than that. This is done in an effort to stabilize
    // the framework from some of the interment issues that we've been seeing related to RPC calls.
    static GLOBAL_CONCURRENCY_LIMITER_LAYER: LazyLock<ConcurrencyLimiterLayer> =
        LazyLock::new(|| ConcurrencyLimiterLayer::new(10));

    let client = ClientBuilder::default()
        .layer(RetryLayer::default())
        .layer(BatchingLayer::new().with_max_batch_size(1000usize))
        .layer(GLOBAL_CONCURRENCY_LIMITER_LAYER.clone())
        .connect(rpc_url)
        .await
        .context("Failed to construct the RPC client")?;

    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .network::<N>()
        .filler(fallback_gas_filler)
        .filler(chain_id_filler)
        .filler(nonce_filler)
        .wallet(wallet)
        .connect_client(client);

    Ok(provider)
}
