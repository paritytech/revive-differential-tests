use std::{ops::ControlFlow, time::Duration};

use anyhow::{Context as _, Result, anyhow};

const EXPONENTIAL_BACKOFF_MAX_WAIT_DURATION: Duration = Duration::from_secs(60);

/// A function that polls for a fallible future for some period of time and errors if it fails to
/// get a result after polling.
///
/// Given a future that returns a [`Result<ControlFlow<O, ()>>`], this function calls the future
/// repeatedly (with some wait period) until the future returns a [`ControlFlow::Break`] or until it
/// returns an [`Err`] in which case the function stops polling and returns the error.
///
/// If the future keeps returning [`ControlFlow::Continue`] and fails to return a [`Break`] within
/// the permitted polling duration then this function returns an [`Err`]
///
/// [`Break`]: ControlFlow::Break
/// [`Continue`]: ControlFlow::Continue
pub async fn poll<F, O>(
    polling_duration: Duration,
    polling_wait_behavior: PollingWaitBehavior,
    mut future: impl FnMut() -> F,
) -> Result<O>
where
    F: Future<Output = Result<ControlFlow<O, ()>>>,
{
    let mut retries = 0;
    let mut total_wait_duration = Duration::ZERO;
    let max_allowed_wait_duration = polling_duration;

    loop {
        if total_wait_duration >= max_allowed_wait_duration {
            break Err(anyhow!(
                "Polling failed after {} retries and a total of {:?} of wait time",
                retries,
                total_wait_duration
            ));
        }

        match future().await.context("Polled future returned an error during polling loop")? {
            ControlFlow::Continue(()) => {
                let next_wait_duration = match polling_wait_behavior {
                    PollingWaitBehavior::Constant(duration) => duration,
                    PollingWaitBehavior::ExponentialBackoff => {
                        Duration::from_secs(2u64.pow(retries))
                            .min(EXPONENTIAL_BACKOFF_MAX_WAIT_DURATION)
                    }
                };
                let next_wait_duration =
                    next_wait_duration.min(max_allowed_wait_duration - total_wait_duration);
                total_wait_duration += next_wait_duration;
                retries += 1;

                tokio::time::sleep(next_wait_duration).await;
            }
            ControlFlow::Break(output) => {
                break Ok(output);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum PollingWaitBehavior {
    Constant(Duration),
    #[default]
    ExponentialBackoff,
}
