//! The alloy crate __requires__ a tokio runtime.
//! We contain any async rust right here.

use std::{any::Any, panic::AssertUnwindSafe, pin::Pin, thread};

use futures::FutureExt;
use once_cell::sync::Lazy;
use tokio::{
    runtime::Builder,
    sync::{mpsc::UnboundedSender, oneshot},
};
use tracing::Instrument;

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
/// use revive_dt_common::concepts::*;
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
        // Note: The blocking executor is a singleton and therefore we store its state in a static
        // so that it's assigned only once. Additionally, when we set the state of the executor we
        // spawn the thread where the async runtime runs.
        static STATE: Lazy<ExecutorState> = Lazy::new(|| {
            tracing::trace!("Initializing the BlockingExecutor state");

            // All communication with the tokio runtime thread happens over mspc channels where the
            // producers here are the threads that want to run async tasks and the consumer here is
            // the tokio runtime thread.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TaskMessage>();

            thread::spawn(move || {
                tracing::info!(
                    thread_id = ?std::thread::current().id(),
                    "Starting async runtime thread"
                );

                let runtime = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create the async runtime");

                runtime.block_on(async move {
                    while let Some(TaskMessage {
                        future: task,
                        response_tx: response_channel,
                    }) = rx.recv().await
                    {
                        tracing::trace!("Received a new future to execute");
                        tokio::spawn(async move {
                            // One of the things that the blocking executor does is that it allows
                            // us to catch panics if they occur. By wrapping the given future in an
                            // AssertUnwindSafe::catch_unwind we are able to catch all panic unwinds
                            // in the given future and convert them into errors.
                            let task = AssertUnwindSafe(task).catch_unwind();

                            let result = task.await;
                            let _ = response_channel.send(result);
                        });
                    }
                })
            });

            ExecutorState { tx }
        });

        // We need to perform blocking synchronous communication between the current thread and the
        // tokio runtime thread with the result of the async computation and the oneshot channels
        // from tokio allows us to do that. The sender side of the channel will be given to the
        // tokio runtime thread to send the result when the computation is completed and the receive
        // side of the channel will be kept with this thread to await for the response of the async
        // task to come back.
        let (response_tx, response_rx) =
            oneshot::channel::<Result<Box<dyn Any + Send>, Box<dyn Any + Send>>>();

        // The tokio runtime thread expects a Future<Output = Box<dyn Any + Send>> + Send to be
        // sent to it to execute. However, this function has a typed Future<Output = R> + Send and
        // therefore we need to change the type of the future to fit what the runtime thread expects
        // in the task message. In doing this conversion, we lose some of the type information since
        // we're converting R => dyn Any. However, we will perform down-casting on the result to
        // convert it back into R.
        let future = Box::pin(
            async move { Box::new(future.await) as Box<dyn Any + Send> }.in_current_span(),
        );

        let task = TaskMessage::new(future, response_tx);
        if let Err(error) = STATE.tx.send(task) {
            tracing::error!(?error, "Failed to send the task to the blocking executor");
            anyhow::bail!("Failed to send the task to the blocking executor: {error:?}")
        }

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

        let result = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(?error, "An error occurred when running the async task");
                anyhow::bail!("An error occurred when running the async task: {error:?}")
            }
        };

        Ok(*result
            .downcast::<R>()
            .expect("An error occurred when downcasting into R. This is a bug"))
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

impl TaskMessage {
    pub fn new(
        future: Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>,
        response_tx: oneshot::Sender<Result<Box<dyn Any + Send>, Box<dyn Any + Send>>>,
    ) -> Self {
        Self {
            future,
            response_tx,
        }
    }
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
            panic!(
                "If this panic causes, well, a panic, then this is an issue. If it's caught then all good!"
            );
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
