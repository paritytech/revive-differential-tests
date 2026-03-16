use tracing::Instrument;

use crate::internal_prelude::*;

/// A layer that allows for automatic retries for getting the receipt.
///
/// There are certain cases where getting the receipt of a committed transaction might fail. In Geth
/// this can happen if the transaction has been committed to the ledger but has not been indexed, in
/// the substrate and revive stack it can also happen for other reasons.
///
/// Therefore, just because the first attempt to get the receipt (after transaction confirmation)
/// has failed it doesn't mean that it will continue to fail. This layer can be added to any alloy
/// provider to allow the provider to retry getting the receipt for some period of time before it
/// considers that a timeout. It uses exponential backoff starting from `initial_backoff` and
/// doubling each attempt up to `max_backoff`. If by the end of the `polling_duration` it was
/// not able to get the receipt successfully then this is considered to be a timeout.
///
/// Additionally, this layer allows for retries for other rpc methods such as all tracing methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryLayer {
    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The initial backoff interval between retries. Doubles after each failed attempt.
    initial_backoff: Duration,

    /// The maximum backoff interval. The backoff will not grow beyond this value.
    max_backoff: Duration,
}

impl RetryLayer {
    pub fn new(
        polling_duration: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            polling_duration,
            initial_backoff,
            max_backoff,
        }
    }

    pub fn with_polling_duration(mut self, polling_duration: Duration) -> Self {
        self.polling_duration = polling_duration;
        self
    }

    pub fn with_initial_backoff(mut self, initial_backoff: Duration) -> Self {
        self.initial_backoff = initial_backoff;
        self
    }

    pub fn with_max_backoff(mut self, max_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;
        self
    }
}

impl Default for RetryLayer {
    fn default() -> Self {
        Self {
            polling_duration: Duration::from_secs(90),
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
        }
    }
}

impl<S> Layer<S> for RetryLayer {
    type Service = RetryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RetryService {
            service: inner,
            polling_duration: self.polling_duration,
            initial_backoff: self.initial_backoff,
            max_backoff: self.max_backoff,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryService<S> {
    /// The internal service.
    service: S,

    /// The amount of time to keep polling for the receipt before considering it a timeout.
    polling_duration: Duration,

    /// The initial backoff interval between retries. Doubles after each failed attempt.
    initial_backoff: Duration,

    /// The maximum backoff interval. The backoff will not grow beyond this value.
    max_backoff: Duration,
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
        let initial_backoff = self.initial_backoff;
        let max_backoff = self.max_backoff;
        let polling_duration = self.polling_duration;

        Box::pin(async move {
            let request = req.as_single().ok_or_else(|| {
                TransportErrorKind::custom_str("Retry layer doesn't support batch requests")
            })?;
            tracing::Span::current().record("request", tracing::field::debug(request));
            let method = request.method();
            let requires_retries = method == "eth_getTransactionReceipt"
                || (method.contains("debug") && method.contains("trace"));

            if !requires_retries {
                debug!(%method, "Retry layer: method does not require retries, forwarding directly");
                return service.call(req).await;
            }

            debug!(%method, ?polling_duration, ?initial_backoff, ?max_backoff, "Retry layer: starting retry loop");
            timeout(polling_duration, async {
                let mut backoff = initial_backoff;
                let mut attempt: u32 = 0;

                loop {
                    attempt += 1;

                    let resp = service.call(req.clone()).await;
                    debug!(?resp, "Obtained a response");
                    let resp = match resp {
                        Ok(resp) => resp,
                        Err(err) => {
                            debug!(%method, attempt, ?backoff, %err, "Retry layer: transport error, retrying");
                            sleep(backoff).await;
                            backoff = (backoff * 2).min(max_backoff);
                            continue;
                        }
                    };
                    let response = resp.as_single().expect("Can't fail");
                    if response.is_error() {
                        debug!(
                            %method,
                            attempt,
                            ?backoff,
                            error = ?response.payload(),
                            "Retry layer: RPC error response, retrying"
                        );
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
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
                        debug!(%method, attempt, "Retry layer: received successful response");
                        return resp;
                    } else {
                        debug!(
                            %method,
                            attempt,
                            ?backoff,
                            ?response,
                            "Retry layer: receipt response was null/unparseable, retrying"
                        );
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                }
            })
            .await
            .map_err(|_| {
                debug!(
                    %method,
                    ?polling_duration,
                    "Retry layer: timed out waiting for successful response"
                );
                TransportErrorKind::custom_str("Timeout when retrying request")
            })
        }.instrument(debug_span!("Handling request", request = tracing::field::Empty)))
    }
}
