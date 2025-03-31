//! Execute transactions in a sync context.

use std::pin::Pin;

use alloy::rpc::types::TransactionReceipt;
use tokio::sync::oneshot;

use crate::TO_TOKIO;
use crate::tokio_runtime::AsyncNodeInteraction;

pub type Task = Pin<Box<dyn Future<Output = anyhow::Result<TransactionReceipt>> + Send>>;

pub(crate) struct Transaction {
    receipt_sender: oneshot::Sender<anyhow::Result<TransactionReceipt>>,
    task: Task,
}

impl AsyncNodeInteraction for Transaction {
    type Output = anyhow::Result<TransactionReceipt>;

    fn split(
        self,
    ) -> (
        Pin<Box<dyn Future<Output = Self::Output> + Send>>,
        oneshot::Sender<Self::Output>,
    ) {
        (self.task, self.receipt_sender)
    }
}

/// Execute some [Task] that returns a [TransactionReceipt].
pub fn execute_transaction(task: Task) -> anyhow::Result<TransactionReceipt> {
    let request_sender = TO_TOKIO.lock().unwrap().transaction_sender.clone();
    let (receipt_sender, receipt_receiver) = oneshot::channel();

    request_sender
        .blocking_send(Transaction {
            receipt_sender,
            task,
        })
        .expect("we are not calling this from an async context");

    receipt_receiver
        .blocking_recv()
        .unwrap_or_else(|error| anyhow::bail!("no receipt received: {error}"))
}
