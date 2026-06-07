use crate::internal_prelude::*;

#[derive(Clone, Debug, Default)]
pub struct DelayedNonceFiller {
    nonces: Arc<DashMap<Address, Arc<Mutex<u64>>>>,
}

impl DelayedNonceFiller {
    /// Assumption: no entry for account = account's nonce is zero. Suitable for retester and all
    /// the assumptions we make.
    pub async fn get_next_nonce(&self, address: Address) -> u64 {
        let entry = self.nonces.entry(address);
        match entry {
            dashmap::Entry::Occupied(ref e) => {
                let mutex = e.get().clone();
                drop(entry);
                let mut nonce = mutex.lock().await;
                *nonce += 1;
                *nonce
            }
            dashmap::Entry::Vacant(e) => {
                e.insert(Arc::new(Mutex::new(0)));
                0
            }
        }
    }
}

impl<N: Network> TxFiller<N> for DelayedNonceFiller {
    // Hack: we need to have the sender's address in the fill function which we can't obtain later.
    type Fillable = Address;

    fn status(&self, tx: &<N as Network>::TransactionRequest) -> FillerControlFlow {
        if tx.nonce().is_some() {
            return FillerControlFlow::Finished;
        }
        if tx.from().is_none() {
            return FillerControlFlow::missing("NonceManager", vec!["from"]);
        }
        FillerControlFlow::Ready
    }

    fn fill_sync(&self, _tx: &mut SendableTx<N>) {}

    // Different behavior: no allocation of nonce now to allow for other fillers to fail.
    async fn prepare<P>(
        &self,
        _: &P,
        tx: &N::TransactionRequest,
    ) -> TransportResult<Self::Fillable> {
        Ok(tx
            .from()
            .expect("qed; this was checked in the 'ready()' function"))
    }

    async fn fill(
        &self,
        fillable: Self::Fillable,
        mut tx: SendableTx<N>,
    ) -> TransportResult<SendableTx<N>> {
        let nonce = self.get_next_nonce(fillable).await;
        if let Some(builder) = tx.as_mut_builder() {
            builder.set_nonce(nonce);
        }
        Ok(tx)
    }
}
