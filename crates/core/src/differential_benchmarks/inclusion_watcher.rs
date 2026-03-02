use crate::internal_prelude::*;

pub struct InclusionWatcher {
    transactions_map: AsyncHashMap<TxHash, ()>,
    stop_notifier: Arc<Notify>,
}

impl InclusionWatcher {
    pub fn new() -> Self {
        Self {
            transactions_map: Default::default(),
            stop_notifier: Arc::new(Notify::new()),
        }
    }

    pub fn await_transaction(&self, tx_hash: TxHash) -> FrameworkFuture<()> {
        self.transactions_map.get(tx_hash)
    }

    pub fn run(
        &self,
        mut blocks_stream: FrameworkStream<MinedBlockInformation>,
    ) -> FrameworkFuture<()> {
        let transactions_map = self.transactions_map.clone();
        let notify = self.stop_notifier.clone();

        Box::pin(async move {
            let task = async move {
                while let Some(mined_block) = blocks_stream.next().await {
                    join_all(
                        mined_block
                            .ethereum_block_information
                            .transaction_hashes
                            .iter()
                            .map(|tx_hash| transactions_map.insert(*tx_hash, ())),
                    )
                    .await;
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
