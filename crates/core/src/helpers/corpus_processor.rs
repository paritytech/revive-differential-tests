//! Shared corpus processing infrastructure.

use std::{
    collections::BTreeSet,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use revive_dt_config::{ConcurrencyConfiguration, FailFastConfiguration};
use revive_dt_report::Reporter;
use tokio::sync::{Notify, RwLock, Semaphore};
use tracing::{Instrument, error, info};

use crate::helpers::CachedCompiler;

/// A guard that invokes `action` when dropped without a terminal status, unless explicitly
/// disarmed via `reported()`.
///
/// When `--fail-fast` aborts in-flight tasks via `select!`, the futures are dropped. This guard
/// ensures that each dropped task can still be reported (e.g. report an ignored event) to the
/// aggregator so that the report is complete.
struct FailFastGuard {
    action: Option<Box<dyn FnOnce() + Send>>,
}

impl FailFastGuard {
    fn reported(&mut self) {
        self.action = None;
    }
}

impl Drop for FailFastGuard {
    fn drop(&mut self) {
        if let Some(action) = self.action.take() {
            action();
        }
    }
}

/// Describes how to process a definition within a corpus.
pub trait CorpusDefinitionProcessor: Sized + 'static {
    /// The definition type produced by the stream.
    type Definition<'a>: 'a;

    /// The result type from processing a definition.
    type ProcessResult;

    /// Additional context-specific state needed for processing.
    type State: Clone;

    /// Processes a single definition.
    fn process_definition<'a>(
        definition: &'a Self::Definition<'a>,
        cached_compiler: &'a CachedCompiler<'a>,
        state: Self::State,
    ) -> impl Future<Output = Result<Self::ProcessResult>>;

    /// Called when a definition is processed successfully.
    fn on_success(_definition: &Self::Definition<'_>, _result: Self::ProcessResult) -> Result<()> {
        Ok(())
    }

    /// Called when a definition fails being processed.
    fn on_failure(_definition: &Self::Definition<'_>, _error: String) -> Result<()> {
        Ok(())
    }

    /// Called when a definition is ignored/aborted.
    fn on_ignored(_definition: &Self::Definition<'_>, _reason: String) -> Result<()> {
        Ok(())
    }

    /// Creates the action to run if this task is aborted due to fail-fast.
    /// Returns `None` if fail-fast is disabled.
    fn create_fail_fast_action(
        definition: &Self::Definition<'_>,
        fail_fast: &FailFastConfiguration,
    ) -> Option<Box<dyn FnOnce() + Send>>;

    /// Creates the tracing span for processing this definition.
    fn create_span(task_id: usize, definition: &Self::Definition<'_>) -> tracing::Span;
}

/// Processes a corpus of definitions using the provided processor.
pub async fn process_corpus<'a, Processor: CorpusDefinitionProcessor>(
    definitions: &'a [Processor::Definition<'a>],
    cached_compiler: &'a CachedCompiler<'a>,
    state: Processor::State,
    concurrency: &ConcurrencyConfiguration,
    fail_fast: &FailFastConfiguration,
    reporter: Reporter,
) {
    let semaphore = concurrency
        .concurrency_limit()
        .map(Semaphore::new)
        .map(Arc::new);
    let running_task_list = Arc::new(RwLock::new(BTreeSet::<usize>::new()));

    let fail_fast_triggered = Arc::new(AtomicBool::new(false));
    let fail_fast_notify = Arc::new(Notify::new());

    // Process all definitions concurrently.
    let driver_task =
        futures::future::join_all(definitions.iter().enumerate().map(|(task_id, definition)| {
            let running_task_list = running_task_list.clone();
            let semaphore = semaphore.clone();
            let fail_fast_triggered = fail_fast_triggered.clone();
            let fail_fast_notify = fail_fast_notify.clone();
            let state = state.clone();
            let span = Processor::create_span(task_id, definition);

            async move {
                let mut fail_fast_guard = FailFastGuard {
                    action: Processor::create_fail_fast_action(definition, fail_fast),
                };

                if fail_fast.fail_fast && fail_fast_triggered.load(Ordering::Relaxed) {
                    Processor::on_ignored(
                        definition,
                        "Skipped due to fail-fast: a prior task failed".to_string(),
                    )
                    .expect("aggregator task is joined later so the receiver is alive");
                    fail_fast_guard.reported();
                    return;
                }

                let permit = match semaphore.as_ref() {
                    Some(semaphore) => match semaphore.acquire().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            Processor::on_ignored(
                                definition,
                                "Skipped due to fail-fast: a prior task failed".to_string(),
                            )
                            .expect("aggregator task is joined later so the receiver is alive");
                            fail_fast_guard.reported();
                            return;
                        }
                    },
                    None => None,
                };

                // Double-check fail-fast after acquiring permit.
                if fail_fast.fail_fast && fail_fast_triggered.load(Ordering::Relaxed) {
                    Processor::on_ignored(
                        definition,
                        "Skipped due to fail-fast: a prior task failed".to_string(),
                    )
                    .expect("aggregator task is joined later so the receiver is alive");
                    fail_fast_guard.reported();
                    drop(permit);
                    return;
                }

                running_task_list.write().await.insert(task_id);

                let result =
                    Processor::process_definition(definition, cached_compiler, state).await;
                match result {
                    Ok(process_result) => {
                        Processor::on_success(definition, process_result)
                            .expect("aggregator task is joined later so the receiver is alive");
                    }
                    Err(error) => {
                        Processor::on_failure(definition, format!("{error:#}"))
                            .expect("aggregator task is joined later so the receiver is alive");

                        if fail_fast.fail_fast {
                            fail_fast_triggered.store(true, Ordering::Relaxed);
                            if let Some(ref sem) = semaphore {
                                sem.close();
                            }
                            fail_fast_notify.notify_one();
                        }
                        error!("Task Failed");
                    }
                }

                fail_fast_guard.reported();

                info!("Finished processing the corpus definition");
                drop(permit);
                running_task_list.write().await.remove(&task_id);
            }
            .instrument(span)
        }));

    // Spawn monitoring task that logs remaining tasks periodically.
    tokio::task::spawn({
        let running_task_list = running_task_list.clone();
        async move {
            loop {
                let remaining_tasks = running_task_list.read().await;
                info!(
                    count = remaining_tasks.len(),
                    ?remaining_tasks,
                    "Remaining Tasks"
                );
                drop(remaining_tasks);
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    });

    // Wait for completion, with optional fail-fast abort.
    if fail_fast.fail_fast {
        tokio::pin!(driver_task);
        tokio::select! {
            biased;
            _ = fail_fast_notify.notified() => {
                info!("Fail-fast triggered, aborting remaining tasks");
            }
            _ = &mut driver_task => {}
        }
    } else {
        driver_task.await;
    }

    info!("Finished processing all corpus definitions");
    reporter
        .report_completion_event()
        .expect("aggregator task is joined later so the receiver is alive");
}
