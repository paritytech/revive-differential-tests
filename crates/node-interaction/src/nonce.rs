use std::pin::Pin;

use alloy::{
    primitives::Address,
    providers::{Provider, ProviderBuilder},
};
use tokio::sync::oneshot;

use crate::{TO_TOKIO, tokio_runtime::AsyncNodeInteraction};

pub type Task = Pin<Box<dyn Future<Output = anyhow::Result<u64>> + Send>>;

pub(crate) struct Nonce {
    sender: oneshot::Sender<anyhow::Result<u64>>,
    task: Task,
}

impl AsyncNodeInteraction for Nonce {
    type Output = anyhow::Result<u64>;

    fn split(
        self,
    ) -> (
        std::pin::Pin<Box<dyn Future<Output = Self::Output> + Send>>,
        oneshot::Sender<Self::Output>,
    ) {
        (self.task, self.sender)
    }
}

/// This is like `trace_transaction`, just for nonces.
pub fn fetch_onchain_nonce(
    connection: String,
    wallet: alloy::network::EthereumWallet,
    address: Address,
) -> anyhow::Result<u64> {
    let sender = TO_TOKIO.lock().unwrap().nonce_sender.clone();

    let (tx, rx) = oneshot::channel();
    let task: Task = Box::pin(async move {
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect(&connection)
            .await?;
        let onchain = provider.get_transaction_count(address).await?;
        Ok(onchain)
    });

    sender
        .blocking_send(Nonce { task, sender: tx })
        .expect("not in async context");

    rx.blocking_recv()
        .unwrap_or_else(|err| anyhow::bail!("nonce fetch failed: {err}"))
}
