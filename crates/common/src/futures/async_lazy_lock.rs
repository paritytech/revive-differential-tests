use std::{
    fmt::Debug,
    future::Future,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Context as _, Result, anyhow};

type InitFn<'a, T> = Box<dyn FnOnce() -> futures::future::BoxFuture<'a, T> + Send + 'a>;

pub struct LazyFutureValue<'a, T> {
    cell: Arc<tokio::sync::OnceCell<T>>,
    init: Arc<Mutex<Option<InitFn<'a, T>>>>,
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
        Func: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = T> + Send + 'a,
    {
        let init: InitFn<'a, T> = Box::new(move || Box::pin(init()));
        Self {
            cell: Arc::new(tokio::sync::OnceCell::new()),
            init: Arc::new(Mutex::new(Some(init))),
        }
    }

    pub async fn get(&self) -> &T {
        self.cell
            .get_or_init(|| {
                let init = self
                    .init
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .expect("LazyFutureValue init is invoked exactly once");
                init()
            })
            .await
    }

    pub fn map<'b, N>(&self, callback: impl FnOnce(&T) -> N + Send + 'b) -> LazyFutureValue<'b, N>
    where
        'a: 'b,
        T: 'b,
        N: Send + Sync + 'b,
    {
        let parent = self.clone();
        LazyFutureValue::new(move || async move { callback(parent.get().await) })
    }
}

impl<'a, T, E> LazyFutureValue<'a, Result<T, E>>
where
    T: Send + Sync + 'a,
    E: Debug + Send + Sync + 'a,
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
