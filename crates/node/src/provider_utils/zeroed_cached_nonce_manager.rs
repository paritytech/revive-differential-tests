use alloy::providers::fillers::NonceManager;
use dashmap::DashMap;
use futures::lock::Mutex;

use crate::internal_prelude::*;

/// A [`NonceManager`] that caches nonces locally and assumes that any account it has not yet seen
/// is completely new.
///
/// This implementation mirrors alloy's [`CachedNonceManager`] with a single deliberate behavioral
/// change: instead of fetching the transaction count from the provider the first time it encounters
/// an address, it assumes the account is brand new and therefore starts the locally cached nonce at
/// `0`. Subsequent calls for the same address are served from the in-memory cache and incremented
/// monotonically — exactly the same as the upstream cached manager.
///
/// # Why this exists
///
/// The differential testing framework provisions fresh nodes for every run and funds wallets
/// through the genesis block. Because those wallets have never submitted a transaction before the
/// test harness starts, the nonce for each one is known to be zero ahead of time. Round-tripping to
/// the RPC just to discover "zero" is wasted latency per newly observed wallet — especially when
/// the framework is sending many concurrent transactions across many nodes simultaneously — and it
/// also fails noisily if the node is not yet ready to answer `eth_getTransactionCount` on the very
/// first request. Assuming the account is new short-circuits that fetch.
///
/// # Divergence from the upstream manager
///
/// The only difference from [`CachedNonceManager`] is how the first-ever call for an address is
/// resolved. The upstream manager calls `provider.get_transaction_count(address)`; this manager
/// returns `0`. All other semantics — the internal sharding via [`DashMap`], the per-address
/// [`Mutex`] guarding updates across await points, and the monotonic increment on subsequent calls
/// — are preserved verbatim.
///
/// [`CachedNonceManager`]: alloy::providers::fillers::CachedNonceManager
#[derive(Clone, Debug, Default)]
pub struct ZeroedCachedNonceManager {
    /// The per-address nonce cache. Each entry stores the most recently handed out nonce for that
    /// account; the entry is seeded lazily the first time the address is observed. [`DashMap`]
    /// gives us lock-free sharded access to the outer map, while the inner [`Mutex`] ensures that
    /// concurrent callers for the same address never hand out the same nonce twice.
    nonces: Arc<DashMap<Address, Arc<Mutex<u64>>>>,
}

#[async_trait::async_trait]
impl NonceManager for ZeroedCachedNonceManager {
    async fn get_next_nonce<P, N>(&self, _: &P, address: Address) -> TransportResult<u64>
    where
        P: Provider<N>,
        N: Network,
    {
        match self.nonces.entry(address) {
            dashmap::Entry::Occupied(occupied_entry) => {
                let nonce = Arc::clone(occupied_entry.get());
                drop(occupied_entry);
                let mut nonce = nonce.lock().await;
                trace!(%address, current_nonce = *nonce, "incrementing nonce");
                *nonce += 1;
                Ok(*nonce)
            }
            dashmap::Entry::Vacant(vacant_entry) => {
                trace!(%address, "assuming unseen account is new; starting nonce at 0");
                vacant_entry.insert(Arc::new(Mutex::new(0)));
                Ok(0)
            }
        }
    }
}
