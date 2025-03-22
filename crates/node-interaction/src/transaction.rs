//! Execute transactions in a sync context.

use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use revive_dt_node::Node;
use tokio::sync::mpsc;

use crate::TO_TOKIO;
use crate::tokio_runtime::AsyncNodeInteraction;

pub(crate) struct Transaction {
    transaction_request: TransactionRequest,
    receipt_sender: mpsc::Sender<anyhow::Result<TransactionReceipt>>,
    connection_string: String,
}

impl AsyncNodeInteraction for Transaction {
    type Output = anyhow::Result<TransactionReceipt>;

    async fn execute_async(self) -> Self::Output {
        let provider = ProviderBuilder::new()
            .connect(&self.connection_string)
            .await?;
        Ok(provider
            .send_transaction(self.transaction_request)
            .await?
            .get_receipt()
            .await?)
    }

    fn output_sender(&self) -> mpsc::Sender<Self::Output> {
        self.receipt_sender.clone()
    }
}

/// Execute the [TransactionRequest] against the `node`.
pub fn execute_transaction<T: Node>(
    transaction_request: TransactionRequest,
    node: &T,
) -> anyhow::Result<TransactionReceipt> {
    let request_sender = TO_TOKIO.lock().unwrap().transaction_sender.clone();
    let (receipt_sender, mut receipt_receiver) = mpsc::channel(1);

    request_sender.blocking_send(Transaction {
        transaction_request,
        receipt_sender,
        connection_string: node.connection_string(),
    })?;

    receipt_receiver
        .blocking_recv()
        .unwrap_or_else(|| anyhow::bail!("no receipt received"))
}
