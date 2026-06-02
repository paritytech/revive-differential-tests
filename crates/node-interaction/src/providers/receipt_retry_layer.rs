use crate::internal_prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetryLayer {
    polling_duration: Duration,
    initial_backoff: Duration,
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
    service: S,
    polling_duration: Duration,
    initial_backoff: Duration,
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

        Box::pin(
            async move {
                let request = req.as_single().ok_or_else(|| {
                    TransportErrorKind::custom_str("Retry layer doesn't support batch requests")
                })?;
                tracing::Span::current().record("request", tracing::field::debug(request));
                let method = request.method();
                let requires_retries = method == "eth_getTransactionReceipt"
                    || (method.contains("debug") && method.contains("trace"));

                if !requires_retries {
                    debug!(
                        %method,
                        "Retry layer: method does not require retries, forwarding directly"
                    );
                    return service.call(req).await;
                }

                debug!(
                    %method,
                    ?polling_duration,
                    ?initial_backoff,
                    ?max_backoff,
                    "Retry layer: starting retry loop"
                );
                let mut attempt_errors = Vec::<String>::new();

                let result = timeout(polling_duration, async {
                    let mut backoff = initial_backoff;
                    let mut attempt = 0_u32;

                    loop {
                        attempt += 1;

                        let resp = service.call(req.clone()).await;
                        debug!(?resp, "Obtained a response");
                        let resp = match resp {
                            Ok(resp) => resp,
                            Err(err) => {
                                let msg = format!("attempt {attempt}: transport error: {err}");
                                debug!(
                                    %method,
                                    attempt,
                                    ?backoff,
                                    %err,
                                    "Retry layer: transport error, retrying"
                                );
                                attempt_errors.push(msg);
                                sleep(backoff).await;
                                backoff = (backoff * 2).min(max_backoff);
                                continue;
                            }
                        };
                        let response = resp.as_single().expect("Can't fail");
                        if let Some(error_response) = response.payload().as_error() {
                            let msg = format!("attempt {attempt}: RPC error: {error_response:?}");
                            debug!(
                                %method,
                                attempt,
                                ?backoff,
                                error = ?error_response,
                                "Retry layer: RPC error response, retrying"
                            );

                            if error_response.message.contains("BasicBlockTooLarge") {
                                break resp;
                            };

                            attempt_errors.push(msg);
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
                            let msg = format!(
                                "attempt {attempt}: receipt response was null/unparseable: \
                                 {response:?}"
                            );
                            debug!(
                                %method,
                                attempt,
                                ?backoff,
                                ?response,
                                "Retry layer: receipt response was null/unparseable, retrying"
                            );
                            attempt_errors.push(msg);
                            sleep(backoff).await;
                            backoff = (backoff * 2).min(max_backoff);
                            continue;
                        }
                    }
                })
                .await;

                result.map_err(|_| {
                    let error_summary = attempt_errors.join("\n  ");
                    error!(
                        %method,
                        ?polling_duration,
                        %error_summary,
                        "Retry layer: timed out waiting for successful response"
                    );
                    TransportErrorKind::custom_str(&format!(
                        "Timeout when retrying {method} after {polling_duration:?}. Attempt \
                         errors:\n  {error_summary}"
                    ))
                })
            }
            .instrument(debug_span!(
                "Handling request",
                request = tracing::field::Empty
            )),
        )
    }
}
