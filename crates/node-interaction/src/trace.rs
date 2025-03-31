//! Trace transactions in a sync context.

use std::pin::Pin;

use alloy::rpc::types::trace::geth::GethTrace;
use tokio::sync::oneshot;

use crate::TO_TOKIO;
use crate::tokio_runtime::AsyncNodeInteraction;

pub type Task = Pin<Box<dyn Future<Output = anyhow::Result<GethTrace>> + Send>>;

pub(crate) struct Trace {
    sender: oneshot::Sender<anyhow::Result<GethTrace>>,
    task: Task,
}

impl AsyncNodeInteraction for Trace {
    type Output = anyhow::Result<GethTrace>;

    fn split(
        self,
    ) -> (
        std::pin::Pin<Box<dyn Future<Output = Self::Output> + Send>>,
        oneshot::Sender<Self::Output>,
    ) {
        (self.task, self.sender)
    }
}

/// Execute some [Task] that return a [GethTrace] result.
pub fn trace_transaction(task: Task) -> anyhow::Result<GethTrace> {
    let task_sender = TO_TOKIO.lock().unwrap().trace_sender.clone();
    let (sender, receiver) = oneshot::channel();

    task_sender
        .blocking_send(Trace { task, sender })
        .expect("we are not calling this from an async context");

    receiver
        .blocking_recv()
        .unwrap_or_else(|error| anyhow::bail!("no trace received: {error}"))
}
