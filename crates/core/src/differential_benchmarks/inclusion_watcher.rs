use std::pin::Pin;

use alloy::primitives::TxHash;
use dashmap::DashMap;
use futures::{Stream, StreamExt};
use revive_dt_report::MinedBlockInformation;
use tokio::{
    select,
    sync::{
        Notify,
        oneshot::{Sender, channel},
    },
};

pub struct InclusionWatcher {
    channels: DashMap<TxHash, Sender<()>>,
    stop_notifier: Notify,
}

impl InclusionWatcher {
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
            stop_notifier: Notify::new(),
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

    pub async fn run(&self, mut blocks_stream: Pin<Box<dyn Stream<Item = MinedBlockInformation>>>) {
        let task = async move {
            while let Some(mined_block) = blocks_stream.next().await {
                mined_block
                    .ethereum_block_information
                    .transaction_hashes
                    .iter()
                    .filter_map(|tx_hash| self.channels.remove(tx_hash))
                    .for_each(|(_, channel)| {
                        let _ = channel.send(());
                    });
            }
        };
        select! {
            _ = self.stop_notifier.notified() => {},
            _ = task => {}
        };
    }

    pub fn stop(&self) {
        self.stop_notifier.notify_waiters();
    }
}
