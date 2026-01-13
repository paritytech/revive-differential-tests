use std::{marker::PhantomData, time::Duration};

use alloy::{
    network::Network,
    primitives::TxHash,
    providers::{Provider, ProviderCall, ProviderLayer, RootProvider},
    transports::{RpcError, TransportErrorKind},
};
use tokio::time::{interval, timeout};

/// A layer that allows for automatic retries for getting the receipt.
///
/// There are certain cases where getting the receipt of a committed transaction might fail. In Geth
/// this can happen if the transaction has been committed to the ledger but has not been indexed, in
/// the substrate and revive stack it can also happen for other reasons.
///
/// Therefore, just because the first attempt to get the receipt (after transaction confirmation)
/// has failed it doesn't mean that it will continue to fail. This layer can be added to any alloy
/// provider to allow the provider to retry getting the receipt for some period of time before it
/// considers that a timeout. It attempts to poll for the receipt for the `polling_duration` with an
/// interval of `polling_interval` between each poll. If by the end of the `polling_duration` it was
/// not able to get the receipt successfully then this is considered to be a timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceiptRetryLayer {
    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The interval of time to wait between each poll for the receipt.
    polling_interval: Duration,
}

impl ReceiptRetryLayer {
    pub fn new(polling_duration: Duration, polling_interval: Duration) -> Self {
        Self {
            polling_duration,
            polling_interval,
        }
    }

    pub fn with_polling_duration(mut self, polling_duration: Duration) -> Self {
        self.polling_duration = polling_duration;
        self
    }

    pub fn with_polling_interval(mut self, polling_interval: Duration) -> Self {
        self.polling_interval = polling_interval;
        self
    }
}

impl Default for ReceiptRetryLayer {
    fn default() -> Self {
        Self {
            polling_duration: Duration::from_secs(90),
            polling_interval: Duration::from_millis(500),
        }
    }
}

impl<P, N> ProviderLayer<P, N> for ReceiptRetryLayer
where
    P: Provider<N>,
    N: Network,
{
    type Provider = ReceiptRetryProvider<P, N>;

    fn layer(&self, inner: P) -> Self::Provider {
        ReceiptRetryProvider::new(self.polling_duration, self.polling_interval, inner)
    }
}

#[derive(Debug, Clone)]
pub struct ReceiptRetryProvider<P, N> {
    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The interval of time to wait between each poll for the receipt.
    polling_interval: Duration,

    /// Inner provider.
    inner: P,

    /// Phantom data
    phantom: PhantomData<N>,
}

impl<P, N> ReceiptRetryProvider<P, N>
where
    P: Provider<N>,
    N: Network,
{
    /// Instantiate a new cache provider.
    pub const fn new(polling_duration: Duration, polling_interval: Duration, inner: P) -> Self {
        Self {
            inner,
            polling_duration,
            polling_interval,
            phantom: PhantomData,
        }
    }
}

impl<P, N> Provider<N> for ReceiptRetryProvider<P, N>
where
    P: Provider<N>,
    N: Network,
{
    #[inline(always)]
    fn root(&self) -> &RootProvider<N> {
        self.inner.root()
    }

    fn get_transaction_receipt(
        &self,
        hash: TxHash,
    ) -> ProviderCall<(TxHash,), Option<<N as Network>::ReceiptResponse>> {
        let client = self.inner.weak_client();
        let polling_duration = self.polling_duration;
        let polling_interval = self.polling_interval;

        ProviderCall::BoxedFuture(Box::pin(async move {
            let client = client
                .upgrade()
                .ok_or_else(|| TransportErrorKind::custom_str("RPC client dropped"))?;

            let receipt = timeout(polling_duration, async move {
                let mut interval = interval(polling_interval);

                loop {
                    let result = client
                        .request::<(TxHash,), Option<<N as Network>::ReceiptResponse>>(
                            "eth_getTransactionReceipt",
                            (hash,),
                        )
                        .await;
                    if let Ok(Some(receipt)) = result {
                        return receipt;
                    }

                    interval.tick().await;
                }
            })
            .await
            .map_err(|_| {
                RpcError::local_usage_str("Timeout when waiting for transaction receipt")
            })?;

            Ok(Some(receipt))
        }))
    }
}
