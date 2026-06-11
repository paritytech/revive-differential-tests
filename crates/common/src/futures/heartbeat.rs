use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval_at};

pub async fn with_heartbeat<F>(future: F, period: Duration, mut on_beat: impl FnMut()) -> F::Output
where
    F: Future,
{
    tokio::pin!(future);

    let mut ticker = interval_at(Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            output = &mut future => return output,
            _ = ticker.tick() => on_beat(),
        }
    }
}

pub trait HeartbeatExt: Future + Sized {
    fn with_heartbeat(
        self,
        period: Duration,
        on_beat: impl FnMut(),
    ) -> impl Future<Output = Self::Output> {
        with_heartbeat(self, period, on_beat)
    }
}

impl<F: Future + Sized> HeartbeatExt for F {}
