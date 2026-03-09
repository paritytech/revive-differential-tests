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
        info!(%tx_hash, "Awaiting transaction inclusion");
        self.transactions_map
            .get(tx_hash)
            .inspect(move |_| info!(%tx_hash, "Transaction has been included"))
            .boxed()
    }

    pub fn run(
        &self,
        mut transaction_inclusion_stream: FrameworkStream<TxHash>,
    ) -> FrameworkFuture<()> {
        let transactions_map = self.transactions_map.clone();
        let notify = self.stop_notifier.clone();

        Box::pin(async move {
            let task = async move {
                while let Some(transaction_hash) = transaction_inclusion_stream.next().await {
                    transactions_map.insert(transaction_hash, ()).await;
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
