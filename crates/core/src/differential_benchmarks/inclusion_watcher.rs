use std::sync::Arc;

use alloy::primitives::TxHash;
use dashmap::DashMap;
use futures::StreamExt;
use revive_dt_common::{
    futures::{FrameworkFuture, FrameworkStream},
    subscriptions::MinedBlockInformation,
};
use tokio::{
    select,
    sync::{
        Notify,
        oneshot::{Sender, channel},
    },
};

pub struct InclusionWatcher {
    channels: Arc<DashMap<TxHash, Sender<()>>>,
    stop_notifier: Arc<Notify>,
}

impl InclusionWatcher {
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
            stop_notifier: Arc::new(Notify::new()),
        }
    }

    pub fn await_transaction(&self, tx_hash: TxHash) -> impl Future<Output = ()> {
        let (tx, rx) = channel::<()>();
        self.channels.insert(tx_hash, tx);
        async move {
            rx.await
                .expect("Can't fail since we don't drop the sender side");
        }
    }

    pub fn run(
        &self,
        mut blocks_stream: FrameworkStream<MinedBlockInformation>,
    ) -> FrameworkFuture<()> {
        let channels = self.channels.clone();
        let notify = self.stop_notifier.clone();

        Box::pin(async move {
            let task = async move {
                while let Some(mined_block) = blocks_stream.next().await {
                    mined_block
                        .ethereum_block_information
                        .transaction_hashes
                        .iter()
                        .filter_map(|tx_hash| channels.remove(tx_hash))
                        .for_each(|(_, channel)| {
                            let _ = channel.send(());
                        });
                }
            };
            select! {
                _ = notify.notified() => {},
                _ = task => {}
            };
        })
    }

    pub fn stop(&self) {
        self.stop_notifier.notify_waiters();
    }
}
