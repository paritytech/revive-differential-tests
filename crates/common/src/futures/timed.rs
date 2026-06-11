use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

pub async fn with_timed<F>(future: F, on_completion: impl FnOnce(Duration)) -> F::Output
where
    F: Future,
{
    let before = Instant::now();
    let rtn = future.await;
    let elapsed = before.elapsed();
    on_completion(elapsed);
    rtn
}

pub trait TimedExt: Future + Sized {
    fn with_timed(
        self,
        on_completion: impl FnOnce(Duration),
    ) -> impl Future<Output = Self::Output> {
        with_timed(self, on_completion)
    }
}

impl<F: Future + Sized> TimedExt for F {}
