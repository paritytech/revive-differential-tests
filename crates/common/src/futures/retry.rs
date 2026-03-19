use std::fmt::Debug;
use std::future::Future;
use std::ops::ControlFlow;
use std::time::Duration;

use tracing::{debug, warn};

/// Retries a future-producing closure with exponential backoff.
///
/// The first attempt runs immediately. On each attempt the closure returns a [`ControlFlow`]:
///
/// * [`ControlFlow::Break(T)`] — the operation succeeded. The function returns `Ok(T)`.
/// * [`ControlFlow::Continue(E)`] — the attempt failed with a retryable error. After waiting for
///   the current delay the delay is doubled and the closure is called again.
///
/// If all attempts are exhausted, the last [`Continue`] error is returned as `Err(E)`.
pub async fn retry_with_exponential_backoff<F, Fut, T, E>(
    max_retries: u32,
    initial_delay: Duration,
    mut f: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ControlFlow<T, E>>,
    E: Debug,
{
    let mut delay = initial_delay;

    for attempt in 0..=max_retries {
        match f().await {
            ControlFlow::Break(value) => {
                if attempt > 0 {
                    debug!(attempt, "Succeeded after retry");
                }
                return Ok(value);
            }
            ControlFlow::Continue(error) if attempt == max_retries => {
                warn!(attempt, max_retries, ?error, "All retry attempts exhausted");
                return Err(error);
            }
            ControlFlow::Continue(error) => {
                warn!(
                    attempt,
                    max_retries,
                    delay_ms = delay.as_millis() as u64,
                    ?error,
                    "Attempt failed, retrying after backoff"
                );
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }

    unreachable!()
}
