//! Implements helpers for node interactions using transactions.
//!
//! The alloy crate is convenient but requires running in a tokio runtime.
//! We contain any async rust right here.

use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use transaction::Transaction;

pub mod transaction;

pub(crate) static TO_TOKIO: Lazy<Mutex<TokioRuntime>> =
    Lazy::new(|| Mutex::new(TokioRuntime::spawn()));

pub struct TokioRuntime {
    pub transaction_sender: mpsc::Sender<Transaction>,
}

impl TokioRuntime {
    pub fn spawn() -> Self {
        let rt = Runtime::new().expect("should be able to create the tokio runtime");
        let (transaction_sender, mut transaction_receiver) = mpsc::channel::<Transaction>(1024);

        thread::spawn(move || {
            rt.block_on(async move {
                while let Some(transaction) = transaction_receiver.recv().await {
                    tokio::task::spawn(async move {
                        let sender = transaction.receipt_sender.clone();
                        let result = transaction.execute().await;
                        if let Err(error) = sender.send(result).await {
                            log::error!("failed to send transaction receipt: {error}");
                        }
                    })
                    .await
                    .expect("should alaways be able to spawn the tokio tasks");
                }
            });
        });

        Self { transaction_sender }
    }
}
