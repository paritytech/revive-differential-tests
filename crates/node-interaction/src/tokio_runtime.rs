//! The alloy crate is convenient but requires a tokio runtime.
//! We contain any async rust right here.

use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::thread;
use tokio::runtime::Runtime;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio::task::JoinError;

use crate::trace::Trace;
use crate::transaction::Transaction;

pub(crate) static TO_TOKIO: Lazy<Mutex<TokioRuntime>> =
    Lazy::new(|| Mutex::new(TokioRuntime::spawn()));

// Common interface for executing async node interactions from a non-async context.
pub(crate) trait AsyncNodeInteraction: Send + 'static {
    type Output: Send + 'static;

    /// Any async calls the task needs to perform go here.
    fn execute_async(self) -> impl std::future::Future<Output = Self::Output> + Send;

    /// Returns the interactions output sender.
    fn output_sender(&self) -> mpsc::Sender<Self::Output>;
}

pub(crate) struct TokioRuntime {
    pub(crate) transaction_sender: mpsc::Sender<Transaction>,
    pub(crate) trace_sender: mpsc::Sender<Trace>,
}

impl TokioRuntime {
    fn spawn() -> Self {
        let rt = Runtime::new().expect("should be able to create the tokio runtime");
        let (transaction_sender, transaction_receiver) = mpsc::channel::<Transaction>(1024);
        let (trace_sender, trace_receiver) = mpsc::channel::<Trace>(1024);

        thread::spawn(move || {
            rt.block_on(async move {
                let transaction_task = spawn(interaction::<Transaction>(transaction_receiver));
                let trace_task = spawn(interaction::<Trace>(trace_receiver));

                if let Err(error) = transaction_task.await {
                    log::error!("tokio transaction task failed: {error}");
                }
                if let Err(error) = trace_task.await {
                    log::error!("tokio trace transaction task failed: {error}");
                }
            });
        });

        Self {
            transaction_sender,
            trace_sender,
        }
    }
}

async fn interaction<T: AsyncNodeInteraction>(
    mut receiver: mpsc::Receiver<T>,
) -> Result<(), JoinError> {
    while let Some(task) = receiver.recv().await {
        spawn(async move {
            let sender = task.output_sender();
            let result = task.execute_async().await;
            if let Err(error) = sender.send(result).await {
                log::error!("failed to send task output: {error}");
            }
        })
        .await?;
    }

    Ok(())
}
