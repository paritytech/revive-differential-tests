use std::sync::Arc;

use alloy::transports::BoxFuture;
use tokio::sync::Semaphore;
use tower::{Layer, Service};

#[derive(Clone, Debug)]
pub struct ConcurrencyLimiterLayer {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyLimiterLayer {
    pub fn new(permit_count: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permit_count)),
        }
    }
}

impl<S> Layer<S> for ConcurrencyLimiterLayer {
    type Service = ConcurrencyLimiterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConcurrencyLimiterService {
            service: inner,
            semaphore: self.semaphore.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ConcurrencyLimiterService<S> {
    service: S,
    semaphore: Arc<Semaphore>,
}

impl<S, Request> Service<Request> for ConcurrencyLimiterService<S>
where
    S: Service<Request> + Send,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let semaphore = self.semaphore.clone();
        let future = self.service.call(req);

        Box::pin(async move {
            let _permit = semaphore
                .acquire()
                .await
                .expect("Semaphore has been closed");
            future.await
        })
    }
}
