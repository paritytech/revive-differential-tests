//! The alloy crate __requires__ a tokio runtime.
//! We contain any async rust right here.

use once_cell::sync::Lazy;
use std::pin::Pin;
use std::sync::Mutex;
use std::thread;
use tokio::runtime::Runtime;
use tokio::spawn;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;

use crate::nonce::Nonce;
use crate::trace::Trace;
use crate::transaction::Transaction;

pub(crate) static TO_TOKIO: Lazy<Mutex<TokioRuntime>> =
    Lazy::new(|| Mutex::new(TokioRuntime::spawn()));

/// Common interface for executing async node interactions from a non-async context.
#[allow(clippy::type_complexity)]
pub(crate) trait AsyncNodeInteraction: Send + 'static {
    type Output: Send;

    //// Returns the task and the output sender.
    fn split(
        self,
    ) -> (
        Pin<Box<dyn Future<Output = Self::Output> + Send>>,
        oneshot::Sender<Self::Output>,
    );
}

pub(crate) struct TokioRuntime {
    pub(crate) transaction_sender: mpsc::Sender<Transaction>,
    pub(crate) trace_sender: mpsc::Sender<Trace>,
    pub(crate) nonce_sender: mpsc::Sender<Nonce>,
}

impl TokioRuntime {
    fn spawn() -> Self {
        let rt = Runtime::new().expect("should be able to create the tokio runtime");
        let (transaction_sender, transaction_receiver) = mpsc::channel::<Transaction>(1024);
        let (trace_sender, trace_receiver) = mpsc::channel::<Trace>(1024);
        let (nonce_sender, nonce_receiver) = mpsc::channel::<Nonce>(1024);

        thread::spawn(move || {
            rt.block_on(async move {
                let transaction_task = spawn(interaction::<Transaction>(transaction_receiver));
                let trace_task = spawn(interaction::<Trace>(trace_receiver));
                let nonce_task = spawn(interaction::<Nonce>(nonce_receiver));

                if let Err(error) = transaction_task.await {
                    tracing::error!("tokio transaction task failed: {error}");
                }
                if let Err(error) = trace_task.await {
                    tracing::error!("tokio trace transaction task failed: {error}");
                }
                if let Err(error) = nonce_task.await {
                    tracing::error!("tokio nonce task failed: {error}");
                }
            });
        });

        Self {
            transaction_sender,
            trace_sender,
            nonce_sender,
        }
    }
}

async fn interaction<T>(mut receiver: mpsc::Receiver<T>) -> Result<(), JoinError>
where
    T: AsyncNodeInteraction,
{
    while let Some(task) = receiver.recv().await {
        spawn(async move {
            let (task, sender) = task.split();
            sender
                .send(task.await)
                .unwrap_or_else(|_| panic!("failed to send task output"));
        });
    }

    Ok(())
}
