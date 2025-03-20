use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use tokio::sync::mpsc;

use crate::TO_TOKIO;

pub struct Transaction {
    pub transaction_request: TransactionRequest,
    pub receipt_sender: mpsc::Sender<anyhow::Result<TransactionReceipt>>,
    pub connection_string: String,
}

impl Transaction {
    pub async fn execute(self) -> anyhow::Result<TransactionReceipt> {
        let provider = ProviderBuilder::new()
            .connect(&self.connection_string)
            .await?;
        Ok(provider
            .send_transaction(self.transaction_request)
            .await?
            .get_receipt()
            .await?)
    }
}

pub fn execute_transaction(
    transaction_request: TransactionRequest,
    connection_string: String,
) -> anyhow::Result<TransactionReceipt> {
    let request_sender = TO_TOKIO.lock().unwrap().transaction_sender.clone();
    let (receipt_sender, mut receipt_receiver) = mpsc::channel(1);

    request_sender.blocking_send(Transaction {
        transaction_request,
        receipt_sender,
        connection_string,
    })?;

    receipt_receiver
        .blocking_recv()
        .unwrap_or_else(|| anyhow::bail!("no receipt received"))
}
