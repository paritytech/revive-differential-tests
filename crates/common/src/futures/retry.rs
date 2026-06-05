use std::future::Future;
use std::ops::ControlFlow;
use std::time::Duration;

pub async fn retry_future_with_exponential_backoff<Func, Fut, T, E>(
    max_retries: usize,
    initial_delay: Duration,
    mut func: Func,
) -> Result<T, E>
where
    Func: FnMut() -> Fut,
    Fut: Future<Output = ControlFlow<Result<T, E>, E>>,
{
    let mut delay = initial_delay;
    for attempt in 0..=max_retries {
        let fut = func();
        let result = fut.await;
        match result {
            ControlFlow::Continue(err) if attempt == max_retries => return Err(err),
            ControlFlow::Continue(_) => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            ControlFlow::Break(brk) => return brk,
        }
    }
    unreachable!()
}
