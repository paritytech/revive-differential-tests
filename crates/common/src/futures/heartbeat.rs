use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval_at};

pub async fn with_heartbeat<F>(
    future: F,
    period: Duration,
    mut on_beat: impl FnMut(Duration),
) -> F::Output
where
    F: Future,
{
    tokio::pin!(future);

    let start = Instant::now();

    let mut ticker = interval_at(start + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            output = &mut future => return output,
            _ = ticker.tick() => on_beat(start.elapsed()),
        }
    }
}

pub trait HeartbeatExt: Future + Sized {
    fn with_heartbeat(
        self,
        period: Duration,
        on_beat: impl FnMut(Duration),
    ) -> impl Future<Output = Self::Output> {
        with_heartbeat(self, period, on_beat)
    }
}

impl<F: Future + Sized> HeartbeatExt for F {}
