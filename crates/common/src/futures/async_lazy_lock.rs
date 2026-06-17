use std::{future::Future, sync::Arc};

use anyhow::{Context as _, Result, anyhow};

pub struct LazyFutureValue<'a, T> {
    cell: Arc<tokio::sync::OnceCell<T>>,
    init: Arc<dyn Fn() -> futures::future::BoxFuture<'a, T> + Send + Sync + 'a>,
}

impl<'a, T> Clone for LazyFutureValue<'a, T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            init: self.init.clone(),
        }
    }
}

impl<'a, T> LazyFutureValue<'a, T>
where
    T: Send + Sync + 'a,
{
    pub fn new<Func, Fut>(init: Func) -> Self
    where
        Func: Fn() -> Fut + Send + Sync + 'a,
        Fut: Future<Output = T> + Send + 'a,
    {
        Self {
            cell: Arc::new(tokio::sync::OnceCell::new()),
            init: Arc::new(move || Box::pin(init())),
        }
    }

    pub async fn get(&self) -> &T {
        self.cell.get_or_init(|| (self.init)()).await
    }

    pub fn map<'b, N>(
        &self,
        callback: impl Fn(&T) -> N + Send + Sync + 'b,
    ) -> LazyFutureValue<'b, N>
    where
        'a: 'b,
        T: 'b,
        N: Send + Sync + 'b,
    {
        let parent = self.clone();
        let callback = Arc::new(callback);
        LazyFutureValue::new(move || {
            let parent = parent.clone();
            let callback = callback.clone();
            async move { callback(parent.get().await) }
        })
    }
}

impl<'a, T, E> LazyFutureValue<'a, Result<T, E>>
where
    T: Send + Sync + 'a,
    E: std::fmt::Debug + Send + Sync + 'a,
{
    pub async fn get_owned_error(&self) -> Result<&T, anyhow::Error> {
        self.get()
            .await
            .as_ref()
            .map_err(|err| anyhow!("Failed to get value: {err:?}"))
    }
}

impl<'a> LazyFutureValue<'a, Result<Arc<alloy::rpc::types::TransactionReceipt>>> {
    pub fn contract_address(&self) -> LazyFutureValue<'a, Result<alloy::primitives::Address>> {
        self.map(move |receipt| {
            receipt
                .as_ref()
                .map_err(|err| anyhow!("Deployment failed: {err}"))
                .and_then(|receipt| {
                    receipt
                        .contract_address
                        .context("qed; this is a deploy transaction")
                })
        })
    }
}
