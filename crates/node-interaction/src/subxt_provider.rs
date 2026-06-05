use crate::internal_prelude::*;

pub(crate) struct RetryingRpcClient<C> {
    inner: C,
}

impl<C> RetryingRpcClient<C> {
    pub(crate) fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C: RpcClientT> RpcClientT for RetryingRpcClient<C> {
    fn request_raw<'a>(
        &'a self,
        method: &'a str,
        params: Option<Box<RawValue>>,
    ) -> RawRpcFuture<'a, Box<RawValue>> {
        Box::pin(run_with_retries(move || {
            self.inner.request_raw(method, params.clone())
        }))
    }

    fn subscribe_raw<'a>(
        &'a self,
        sub: &'a str,
        params: Option<Box<RawValue>>,
        unsub: &'a str,
    ) -> RawRpcFuture<'a, RawRpcSubscription> {
        Box::pin(run_with_retries(move || {
            self.inner.subscribe_raw(sub, params.clone(), unsub)
        }))
    }
}

async fn run_with_retries<T, Func, Fut>(mut func: Func) -> Result<T, RpcsError>
where
    Func: FnMut() -> Fut,
    Fut: Future<Output = Result<T, RpcsError>>,
{
    retry_future_with_exponential_backoff(10, Duration::from_millis(10), || {
        let fut = func();
        async move {
            let result = fut.await;
            match result {
                Ok(value) => ControlFlow::Break(Ok(value)),
                Err(e @ (RpcsError::Client(..) | RpcsError::DisconnectedWillReconnect(..))) => {
                    ControlFlow::Continue(e)
                }
                e @ Err(
                    RpcsError::Decode(..)
                    | RpcsError::User(..)
                    | RpcsError::Deserialization(..)
                    | RpcsError::InsecureUrl(..),
                ) => ControlFlow::Break(e),
                // We need to include this here since the error type is `non_exhaustive` so we
                // can't match over all of the variants that the error type can contain.
                e @ Err(_) => ControlFlow::Break(e),
            }
        }
    })
    .await
}

pub fn new_substrate_client(
    url: impl Into<String>,
) -> StaticFuture<Result<OnlineClient<PolkadotConfig>>> {
    let url = url.into();
    Box::pin(async move {
        validate_url_is_secure(&url).context("The substrate RPC URL is insecure")?;
        let inner = reconnecting_rpc_client::RpcClient::builder()
            .build(&url)
            .await
            .context("Failed to build the reconnecting substrate RPC client")?;
        let rpc_client = SubxtRpcClient::new(RetryingRpcClient::new(inner));
        OnlineClient::<PolkadotConfig>::from_rpc_client(rpc_client)
            .await
            .context("Failed to construct the substrate online client")
    })
}
