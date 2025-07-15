//! The alloy crate __requires__ a tokio runtime.
//! We contain any async rust right here.

use std::{any::Any, panic::AssertUnwindSafe, pin::Pin, thread};

use futures::FutureExt;
use once_cell::sync::Lazy;
use tokio::{
    runtime::Builder,
    sync::{mpsc::UnboundedSender, oneshot},
};

/// A blocking async executor.
///
/// This struct exposes the abstraction of a blocking async executor. It is a global and static
/// executor which means that it doesn't require for new instances of it to be created, it's a
/// singleton and can be accessed by any thread that wants to perform some async computation on the
/// blocking executor thread.
///
/// The API of the blocking executor is created in a way so that it's very natural, simple to use,
/// and unbounded to specific tasks or return types. The following is an example of using this
/// executor to drive an async computation:
///
/// ```rust
/// use revive_dt_node_interaction::*;
///
/// fn blocking_function() {
///     let result = BlockingExecutor::execute(async move {
///         tokio::time::sleep(std::time::Duration::from_secs(1)).await;
///         0xFFu8
///     })
///     .expect("Computation failed");
///
///     assert_eq!(result, 0xFF);
/// }
/// ```
///
/// Users get to pass in their async tasks without needing to worry about putting them in a [`Box`],
/// [`Pin`], needing to perform down-casting, or the internal channel mechanism used by the runtime.
/// To the user, it just looks like a function that converts some async code into sync code.
///
/// This struct also handled panics that occur in the passed futures and converts them into errors
/// that can be handled by the user. This is done to allow the executor to be robust.
///
/// Internally, the executor communicates with the tokio runtime thread through channels which carry
/// the [`TaskMessage`] and the results of the execution.
pub struct BlockingExecutor;

impl BlockingExecutor {
    pub fn execute<R>(future: impl Future<Output = R> + Send + 'static) -> Result<R, anyhow::Error>
    where
        R: Send + 'static,
    {
        static STATE: Lazy<ExecutorState> = Lazy::new(|| {
            tracing::trace!("Initializing the BlockingExecutor state");

            // Creating a multiple-producer-single-consumer channel which allows all of the other
            // threads to communicate with this one async runtime thread.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TaskMessage>();

            // We spawn a new thread which will house the async runtime and will always be listening
            // for new tasks coming in and executing them as they come in.
            thread::spawn(move || {
                let runtime = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create the async runtime");

                runtime.block_on(async move {
                    // Keep getting new task messages from all of the other threads.
                    while let Some(TaskMessage {
                        future: task,
                        response_tx: response_channel,
                    }) = rx.recv().await
                    {
                        // Spawn off each job so that the receive loop is not blocked.
                        tracing::trace!("Received a new future to execute");
                        tokio::spawn(async move {
                            let task = AssertUnwindSafe(task).catch_unwind();
                            let result = task.await;
                            let _ = response_channel.send(result);
                        });
                    }
                })
            });

            // Creating the state of the async runtime.
            ExecutorState { tx }
        });

        // Creating a one-shot channel for this task that will be used to send and receive the
        // response of the task.
        let (response_tx, response_rx) =
            oneshot::channel::<Result<Box<dyn Any + Send>, Box<dyn Any + Send>>>();

        // Converting the future from the shape that it is in into the shape that the runtime is
        // expecting it to be in.
        let future = Box::pin(async move { Box::new(future.await) as Box<dyn Any + Send> });

        // Sending the task to the runtime,
        let task = TaskMessage {
            future,
            response_tx,
        };

        if let Err(error) = STATE.tx.send(task) {
            tracing::error!(?error, "Failed to send the task to the blocking executor");
            anyhow::bail!("Failed to send the task to the blocking executor: {error:?}")
        }

        // Await for the result of the execution to come back over the channel.
        let result = match response_rx.blocking_recv() {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(
                    ?error,
                    "Failed to get the response from the blocking executor"
                );
                anyhow::bail!("Failed to get the response from the blocking executor: {error:?}")
            }
        };

        match result.map(|result| {
            *result
                .downcast::<R>()
                .expect("Type mismatch in the downcast")
        }) {
            Ok(result) => Ok(result),
            Err(error) => {
                tracing::error!(
                    ?error,
                    "Failed to downcast the returned result into the expected type"
                );
                anyhow::bail!(
                    "Failed to downcast the returned result into the expected type: {error:?}"
                )
            }
        }
    }
}

/// Represents the state of the async runtime. This runtime is designed to be a singleton runtime
/// which means that in the current running program there's just a single thread that has an async
/// runtime.
struct ExecutorState {
    /// The sending side of the task messages channel. This is used by all of the other threads to
    /// communicate with the async runtime thread.
    tx: UnboundedSender<TaskMessage>,
}

/// Represents a message that contains an asynchronous task that's to be executed by the runtime
/// as well as a way for the runtime to report back on the result of the execution.
struct TaskMessage {
    /// The task that's being requested to run. This is a future that returns an object that does
    /// implement [`Any`] and [`Send`] to allow it to be sent between the requesting thread and the
    /// async thread.
    future: Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>,

    /// A one shot sender channel where the sender of the task is expecting to hear back on the
    /// result of the task.
    response_tx: oneshot::Sender<Result<Box<dyn Any + Send>, Box<dyn Any + Send>>>,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn simple_future_works() {
        // Act
        let result = BlockingExecutor::execute(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            0xFFu8
        })
        .unwrap();

        // Assert
        assert_eq!(result, 0xFFu8);
    }

    #[test]
    #[allow(unreachable_code, clippy::unreachable)]
    fn panics_in_futures_are_caught() {
        // Act
        let result = BlockingExecutor::execute(async move {
            panic!("This is a panic!");
            0xFFu8
        });

        // Assert
        assert!(result.is_err());

        // Act
        let result = BlockingExecutor::execute(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            0xFFu8
        })
        .unwrap();

        // Assert
        assert_eq!(result, 0xFFu8)
    }
}
