use std::time::Duration;

use alloy::{
    network::{AnyNetwork, Network},
    rpc::json_rpc::{RequestPacket, ResponsePacket},
    transports::{TransportError, TransportErrorKind, TransportFut},
};
use tokio::time::{interval, timeout};
use tower::{Layer, Service};

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
///
/// Additionally, this layer allows for retries for other rpc methods such as all tracing methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryLayer {
    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The interval of time to wait between each poll for the receipt.
    polling_interval: Duration,
}

impl RetryLayer {
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

impl Default for RetryLayer {
    fn default() -> Self {
        Self {
            polling_duration: Duration::from_secs(90),
            polling_interval: Duration::from_millis(500),
        }
    }
}

impl<S> Layer<S> for RetryLayer {
    type Service = RetryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RetryService {
            service: inner,
            polling_duration: self.polling_duration,
            polling_interval: self.polling_interval,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryService<S> {
    /// The internal service.
    service: S,

    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The interval of time to wait between each poll for the receipt.
    polling_interval: Duration,
}

impl<S> Service<RequestPacket> for RetryService<S>
where
    S: Service<RequestPacket, Future = TransportFut<'static>, Error = TransportError>
        + Send
        + 'static
        + Clone,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    #[allow(clippy::nonminimal_bool)]
    fn call(&mut self, req: RequestPacket) -> Self::Future {
        type ReceiptOutput = <AnyNetwork as Network>::ReceiptResponse;

        let mut service = self.service.clone();
        let polling_interval = self.polling_interval;
        let polling_duration = self.polling_duration;

        Box::pin(async move {
            let request = req.as_single().ok_or_else(|| {
                TransportErrorKind::custom_str("Retry layer doesn't support batch requests")
            })?;
            let method = request.method();
            let requires_retries = method == "eth_getTransactionReceipt"
                || (method.contains("debug") && method.contains("trace"));

            if !requires_retries {
                return service.call(req).await;
            }

            timeout(polling_duration, async {
                let mut interval = interval(polling_interval);

                loop {
                    interval.tick().await;

                    let Ok(resp) = service.call(req.clone()).await else {
                        continue;
                    };
                    let response = resp.as_single().expect("Can't fail");
                    if response.is_error() {
                        continue;
                    }

                    if method == "eth_getTransactionReceipt"
                        && response
                            .payload()
                            .clone()
                            .deserialize_success::<ReceiptOutput>()
                            .ok()
                            .and_then(|resp| resp.try_into_success().ok())
                            .is_some()
                        || method != "eth_getTransactionReceipt"
                    {
                        return resp;
                    } else {
                        continue;
                    }
                }
            })
            .await
            .map_err(|_| TransportErrorKind::custom_str("Timeout when retrying request"))
        })
    }
}
