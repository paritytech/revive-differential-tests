//! Trace transactions in a sync context.

use alloy::primitives::TxHash;
use alloy::providers::ProviderBuilder;
use alloy::providers::ext::DebugApi;
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::{GethDebugTracingOptions, GethTrace};
use tokio::sync::mpsc;

use crate::TO_TOKIO;
use crate::tokio_runtime::AsyncNodeInteraction;

pub(crate) struct Trace {
    transaction_hash: TxHash,
    options: GethDebugTracingOptions,
    geth_trace_sender: mpsc::Sender<anyhow::Result<GethTrace>>,
    connection_string: String,
}

impl AsyncNodeInteraction for Trace {
    type Output = anyhow::Result<GethTrace>;

    async fn execute_async(self) -> Self::Output {
        let provider = ProviderBuilder::new()
            .connect(&self.connection_string)
            .await?;
        Ok(provider
            .debug_trace_transaction(self.transaction_hash, self.options)
            .await?)
    }

    fn output_sender(&self) -> mpsc::Sender<Self::Output> {
        self.geth_trace_sender.clone()
    }
}

/// Trace the transaction in [TransactionReceipt] against the `node`,
/// using the provided [GethDebugTracingOptions].
pub fn trace_transaction(
    transaction_receipt: TransactionReceipt,
    options: GethDebugTracingOptions,
    connection_string: String,
) -> anyhow::Result<GethTrace> {
    let trace_sender = TO_TOKIO.lock().unwrap().trace_sender.clone();
    let (geth_trace_sender, mut geth_trace_receiver) = mpsc::channel(1);

    trace_sender.blocking_send(Trace {
        transaction_hash: transaction_receipt.transaction_hash,
        options,
        geth_trace_sender,
        connection_string,
    })?;

    geth_trace_receiver
        .blocking_recv()
        .unwrap_or_else(|| anyhow::bail!("no receipt received"))
}
