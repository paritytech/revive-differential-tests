use crate::internal_prelude::*;

use dashmap::DashSet;

pub struct InclusionWatcher {
    channels: Arc<DashMap<TxHash, oneshot::Sender<()>>>,
    /// Tracks all transaction hashes observed in mined blocks. This is used to handle the race
    /// condition where a block containing a transaction is processed before `await_transaction` is
    /// called for that transaction.
    seen_tx_hashes: Arc<DashSet<TxHash>>,
    stop_notifier: Arc<Notify>,
}

impl InclusionWatcher {
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
            seen_tx_hashes: Default::default(),
            stop_notifier: Arc::new(Notify::new()),
        }
    }

    pub fn await_transaction(&self, tx_hash: TxHash) -> impl Future<Output = ()> {
        // Fast path: the transaction was already observed in a mined block before we registered
        // our interest. Return immediately.
        if self.seen_tx_hashes.contains(&tx_hash) {
            return futures::future::Either::Left(async {});
        }

        let (tx, rx) = oneshot::channel::<()>();
        self.channels.insert(tx_hash, tx);

        // Double-check after inserting into channels to close the race window: between the
        // `contains` check above and the `insert`, the block-processing task may have added this
        // tx hash to `seen_tx_hashes` without finding it in `channels` (since we hadn't inserted
        // yet). If that happened, signal immediately.
        if self.seen_tx_hashes.contains(&tx_hash) {
            if let Some((_, sender)) = self.channels.remove(&tx_hash) {
                let _ = sender.send(());
            }
        }

        futures::future::Either::Right(async move {
            rx.await
                .expect("Can't fail since we don't drop the sender side");
        })
    }

    pub fn run(
        &self,
        mut blocks_stream: FrameworkStream<MinedBlockInformation>,
    ) -> FrameworkFuture<()> {
        let channels = self.channels.clone();
        let seen = self.seen_tx_hashes.clone();
        let notify = self.stop_notifier.clone();

        Box::pin(async move {
            let task = async move {
                while let Some(mined_block) = blocks_stream.next().await {
                    mined_block
                        .ethereum_block_information
                        .transaction_hashes
                        .iter()
                        .for_each(|tx_hash| {
                            // Record that we've seen this tx hash FIRST, then check channels.
                            // This ordering ensures that `await_transaction`'s double-check will
                            // catch any tx registered between the block processing and the insert.
                            seen.insert(*tx_hash);
                            if let Some((_, channel)) = channels.remove(tx_hash) {
                                let _ = channel.send(());
                            }
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
