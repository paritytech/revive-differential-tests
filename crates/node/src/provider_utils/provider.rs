use std::{ops::ControlFlow, sync::LazyLock, time::Duration};

use alloy::{
    network::{Ethereum, Network, NetworkWallet, TransactionBuilder4844},
    providers::{
        Identity, PendingTransactionBuilder, Provider, ProviderBuilder, RootProvider,
        fillers::{ChainIdFiller, FillProvider, JoinFill, NonceFiller, TxFiller, WalletFiller},
    },
    rpc::client::ClientBuilder,
};
use anyhow::{Context, Result};
use revive_dt_common::futures::{PollingWaitBehavior, poll};
use tracing::{Instrument, debug, info, info_span};

use crate::provider_utils::{ConcurrencyLimiterLayer, FallbackGasFiller};

pub type ConcreteProvider<N, W> = FillProvider<
    JoinFill<
        JoinFill<JoinFill<JoinFill<Identity, FallbackGasFiller>, ChainIdFiller>, NonceFiller>,
        WalletFiller<W>,
    >,
    RootProvider<N>,
    N,
>;

pub async fn construct_concurrency_limited_provider<N, W>(
    rpc_url: &str,
    fallback_gas_filler: FallbackGasFiller,
    chain_id_filler: ChainIdFiller,
    nonce_filler: NonceFiller,
    wallet: W,
) -> Result<ConcreteProvider<N, W>>
where
    N: Network<TransactionRequest: TransactionBuilder4844>,
    W: NetworkWallet<N>,
    Identity: TxFiller<N>,
    FallbackGasFiller: TxFiller<N>,
    ChainIdFiller: TxFiller<N>,
    NonceFiller: TxFiller<N>,
    WalletFiller<W>: TxFiller<N>,
{
    // This is a global limit on the RPC concurrency that applies to all of the providers created
    // by the framework. With this limit, it means that we can have a maximum of N concurrent
    // requests at any point of time and no more than that. This is done in an effort to stabilize
    // the framework from some of the interment issues that we've been seeing related to RPC calls.
    static GLOBAL_CONCURRENCY_LIMITER_LAYER: LazyLock<ConcurrencyLimiterLayer> =
        LazyLock::new(|| ConcurrencyLimiterLayer::new(500));

    let client = ClientBuilder::default()
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

pub async fn execute_transaction<N, W>(
    provider: ConcreteProvider<N, W>,
    transaction: N::TransactionRequest,
) -> Result<N::ReceiptResponse>
where
    N: Network<
            TransactionRequest: TransactionBuilder4844,
            TxEnvelope = <Ethereum as Network>::TxEnvelope,
        >,
    W: NetworkWallet<N>,
    Identity: TxFiller<N>,
    FallbackGasFiller: TxFiller<N>,
    ChainIdFiller: TxFiller<N>,
    NonceFiller: TxFiller<N>,
    WalletFiller<W>: TxFiller<N>,
{
    let sendable_transaction = provider
        .fill(transaction)
        .await
        .context("Failed to fill transaction")?;

    let transaction_envelope = sendable_transaction
        .try_into_envelope()
        .context("Failed to convert transaction into an envelope")?;
    let tx_hash = *transaction_envelope.tx_hash();

    let mut pending_transaction = match provider.send_tx_envelope(transaction_envelope).await {
        Ok(pending_transaction) => pending_transaction,
        Err(error) => {
            let error_string = error.to_string();

            if error_string.contains("Transaction Already Imported") {
                PendingTransactionBuilder::<N>::new(provider.root().clone(), tx_hash)
            } else {
                return Err(error).context(format!("Failed to submit transaction {tx_hash}"));
            }
        }
    };
    debug!(%tx_hash, "Submitted Transaction");

    pending_transaction.set_timeout(Some(Duration::from_secs(240)));
    let tx_hash = pending_transaction.watch().await.context(format!(
        "Transaction inclusion watching timeout for {tx_hash}"
    ))?;

    poll(
        Duration::from_secs(60),
        PollingWaitBehavior::Constant(Duration::from_secs(3)),
        || {
            let provider = provider.clone();

            async move {
                match provider.get_transaction_receipt(tx_hash).await {
                    Ok(Some(receipt)) => {
                        info!("Found the transaction receipt");
                        Ok(ControlFlow::Break(receipt))
                    }
                    _ => Ok(ControlFlow::Continue(())),
                }
            }
        },
    )
    .instrument(info_span!("Polling for receipt", %tx_hash))
    .await
    .context(format!("Polling for receipt failed for {tx_hash}"))
}
