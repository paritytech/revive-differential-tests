//! The main entry point into differential testing.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufWriter, Write, stderr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use ansi_term::{ANSIStrings, Color};
use anyhow::Context as _;
use futures::StreamExt;
use indexmap::IndexMap;
use revive_dt_common::types::PrivateKeyAllocator;
use revive_dt_core::Platform;
use revive_dt_format::corpus::Corpus;
use tokio::sync::{Mutex, Notify, RwLock, Semaphore};
use tracing::{Instrument, error, info, info_span, instrument};

use revive_dt_config::{Context, OutputFormat, Test};
use revive_dt_report::{Reporter, ReporterEvent, TestCaseStatus, TestSpecificReporter};

use crate::{
    differential_tests::Driver,
    helpers::{
        CachedCompiler, NodePool, TestCaseIgnoreResolvedConfiguration,
        create_test_definitions_stream,
    },
};

/// A guard that reports a test as ignored when dropped without a terminal status.
///
/// When `--fail-fast` aborts in-flight tests via `select!`, the futures are dropped. This guard
/// ensures that each dropped test still sends an ignored event to the aggregator so the report
/// is complete.
struct FailFastGuard {
    reporter: Option<TestSpecificReporter>,
}

impl FailFastGuard {
    fn reported(&mut self) {
        self.reporter = None;
    }
}

impl Drop for FailFastGuard {
    fn drop(&mut self) {
        if let Some(ref reporter) = self.reporter {
            let _ = reporter
                .report_test_ignored_event("Aborted due to fail-fast".to_string(), IndexMap::new());
        }
    }
}

/// Handles the differential testing executing it according to the information defined in the
/// context
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_differential_tests(context: Test, reporter: Reporter) -> anyhow::Result<()> {
    let reporter_clone = reporter.clone();

    // Discover all of the metadata files that are defined in the context.
    let corpus = context
        .corpus
        .test_specifiers
        .clone()
        .into_iter()
        .try_fold(Corpus::default(), Corpus::with_test_specifier)
        .context("Failed to parse the test corpus")?;
    info!(
        len = corpus.metadata_file_count(),
        "Discovered metadata files"
    );

    // Discover the list of platforms that the tests should run on based on the context.
    let platforms = context
        .platforms
        .platforms
        .iter()
        .copied()
        .map(Into::<&dyn Platform>::into)
        .collect::<Vec<_>>();

    // Starting the nodes of the various platforms specified in the context.
    let platforms_and_nodes = {
        let mut map = BTreeMap::new();

        for platform in platforms.iter() {
            let platform_identifier = platform.platform_identifier();

            let context = Context::Test(Box::new(context.clone()));
            let node_pool = NodePool::new(context, *platform)
                .await
                .inspect_err(|err| {
                    error!(
                        ?err,
                        %platform_identifier,
                        "Failed to initialize the node pool for the platform."
                    )
                })
                .context("Failed to initialize the node pool")?;

            map.insert(platform_identifier, (*platform, node_pool));
        }

        map
    };
    info!("Spawned the platform nodes");

    // Preparing test definitions.
    let test_case_ignore_configuration =
        TestCaseIgnoreResolvedConfiguration::try_from(context.ignore.clone())?;
    let full_context = Context::Test(Box::new(context.clone()));
    let test_definitions = create_test_definitions_stream(
        &full_context,
        &corpus,
        &platforms_and_nodes,
        &test_case_ignore_configuration,
        reporter.clone(),
    )
    .await
    .collect::<Vec<_>>()
    .await;
    info!(len = test_definitions.len(), "Created test definitions");

    // Creating everything else required for the driver to run.
    let cached_compiler = CachedCompiler::new(
        context
            .working_directory
            .working_directory
            .as_path()
            .join("compilation_cache"),
        context.compilation.invalidate_cache,
    )
    .await
    .map(Arc::new)
    .context("Failed to initialize cached compiler")?;
    let private_key_allocator = Arc::new(Mutex::new(PrivateKeyAllocator::new(
        context.wallet.highest_private_key_exclusive(),
    )));

    // Creating the driver and executing all of the steps.
    let semaphore = context
        .concurrency
        .concurrency_limit()
        .map(Semaphore::new)
        .map(Arc::new);
    let running_task_list = Arc::new(RwLock::new(BTreeSet::<usize>::new()));
    let fail_fast_triggered = Arc::new(AtomicBool::new(false));
    let fail_fast_notify = Arc::new(Notify::new());
    let driver_task = futures::future::join_all(test_definitions.iter().enumerate().map(
        |(test_id, test_definition)| {
            let running_task_list = running_task_list.clone();
            let semaphore = semaphore.clone();
            let fail_fast_triggered = fail_fast_triggered.clone();
            let fail_fast_notify = fail_fast_notify.clone();
            let fail_fast = context.fail_fast.fail_fast;

            let private_key_allocator = private_key_allocator.clone();
            let cached_compiler = cached_compiler.clone();
            let mode = test_definition.mode.clone();
            let span = info_span!(
                "Executing Test Case",
                test_id,
                metadata_file_path = %test_definition.metadata_file_path.display(),
                case_idx = %test_definition.case_idx,
                mode = %mode,
            );
            async move {
                let mut fail_fast_guard = FailFastGuard {
                    reporter: fail_fast.then(|| test_definition.reporter.clone()),
                };

                if fail_fast && fail_fast_triggered.load(Ordering::Relaxed) {
                    test_definition
                        .reporter
                        .report_test_ignored_event(
                            "Skipped due to fail-fast: a prior test failed".to_string(),
                            IndexMap::new(),
                        )
                        .expect("aggregator task is joined later so the receiver is alive");
                    fail_fast_guard.reported();
                    return;
                }

                let permit = match semaphore.as_ref() {
                    Some(semaphore) => match semaphore.acquire().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            test_definition
                                .reporter
                                .report_test_ignored_event(
                                    "Skipped due to fail-fast: a prior test failed".to_string(),
                                    IndexMap::new(),
                                )
                                .expect("aggregator task is joined later so the receiver is alive");
                            fail_fast_guard.reported();
                            return;
                        }
                    },
                    None => None,
                };

                if fail_fast && fail_fast_triggered.load(Ordering::Relaxed) {
                    test_definition
                        .reporter
                        .report_test_ignored_event(
                            "Skipped due to fail-fast: a prior test failed".to_string(),
                            IndexMap::new(),
                        )
                        .expect("aggregator task is joined later so the receiver is alive");
                    fail_fast_guard.reported();
                    drop(permit);
                    return;
                }

                running_task_list.write().await.insert(test_id);
                let driver = match Driver::new_root(
                    test_definition,
                    private_key_allocator,
                    &cached_compiler,
                )
                .await
                {
                    Ok(driver) => driver,
                    Err(error) => {
                        test_definition
                            .reporter
                            .report_test_failed_event(format!("{error:#}"))
                            .expect("Can't fail");
                        fail_fast_guard.reported();
                        if fail_fast {
                            fail_fast_triggered.store(true, Ordering::Relaxed);
                            if let Some(ref sem) = semaphore {
                                sem.close();
                            }
                            fail_fast_notify.notify_one();
                        }
                        error!("Test Case Failed");
                        drop(permit);
                        running_task_list.write().await.remove(&test_id);
                        return;
                    }
                };
                info!("Created the driver for the test case");

                match driver.execute_all().await {
                    Ok(steps_executed) => test_definition
                        .reporter
                        .report_test_succeeded_event(steps_executed)
                        .expect("Can't fail"),
                    Err(error) => {
                        test_definition
                            .reporter
                            .report_test_failed_event(format!("{error:#}"))
                            .expect("Can't fail");
                        if fail_fast {
                            fail_fast_triggered.store(true, Ordering::Relaxed);
                            if let Some(ref sem) = semaphore {
                                sem.close();
                            }
                            fail_fast_notify.notify_one();
                        }
                        error!("Test Case Failed");
                    }
                };
                fail_fast_guard.reported();
                info!("Finished the execution of the test case");
                drop(permit);
                running_task_list.write().await.remove(&test_id);
            }
            .instrument(span)
        },
    ));
    let cli_reporting_task =
        tokio::spawn(start_cli_reporting_task(context.output_format.output_format, reporter));

    tokio::task::spawn(async move {
        loop {
            let remaining_tasks = running_task_list.read().await;
            info!(
                count = remaining_tasks.len(),
                ?remaining_tasks,
                "Remaining Tests"
            );
            drop(remaining_tasks);
            tokio::time::sleep(Duration::from_secs(10)).await
        }
    });

    if context.fail_fast.fail_fast {
        tokio::pin!(driver_task);
        tokio::select! {
            biased;
            _ = fail_fast_notify.notified() => {
                info!("Fail-fast triggered, aborting remaining tests");
            }
            _ = &mut driver_task => {}
        }
    } else {
        driver_task.await;
    }

    info!("Finished executing all test cases");
    reporter_clone
        .report_completion_event()
        .expect("Can't fail");
    drop(reporter_clone);

    cli_reporting_task
        .await
        .expect("CLI reporting task panicked");

    Ok(())
}

#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(output_format: OutputFormat, reporter: Reporter) {
    let mut aggregator_events_rx = reporter.subscribe().await.expect("Can't fail");
    drop(reporter);

    let start = Instant::now();

    let mut global_success_count = 0;
    let mut global_failure_count = 0;
    let mut global_ignore_count = 0;

    let mut buf = BufWriter::new(stderr());
    while let Ok(event) = aggregator_events_rx.recv().await {
        let ReporterEvent::MetadataFileSolcModeCombinationExecutionCompleted {
            metadata_file_path,
            mode,
            case_status,
        } = event
        else {
            continue;
        };

        match output_format {
            OutputFormat::Legacy => {
                let _ = writeln!(buf, "{} - {}", mode, metadata_file_path.display());
                for (case_idx, case_status) in case_status.into_iter() {
                    let _ = write!(buf, "\tCase Index {case_idx:>3}: ");
                    let _ = match case_status {
                        TestCaseStatus::Succeeded { steps_executed } => {
                            global_success_count += 1;
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Green.bold().paint("Case Succeeded"),
                                    Color::Green
                                        .paint(format!(" - Steps Executed: {steps_executed}")),
                                ])
                            )
                        }
                        TestCaseStatus::Failed { reason } => {
                            global_failure_count += 1;
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Red.bold().paint("Case Failed"),
                                    Color::Red.paint(format!(" - Reason: {}", reason.trim())),
                                ])
                            )
                        }
                        TestCaseStatus::Ignored { reason, .. } => {
                            global_ignore_count += 1;
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Yellow.bold().paint("Case Ignored"),
                                    Color::Yellow.paint(format!(" - Reason: {}", reason.trim())),
                                ])
                            )
                        }
                    };
                }
                let _ = writeln!(buf);
            }
            OutputFormat::CargoTestLike => {
                writeln!(
                    buf,
                    "\t{} {} - {}\n",
                    Color::Green.paint("Running"),
                    metadata_file_path.display(),
                    mode
                )
                .unwrap();

                let mut success_count = 0;
                let mut failure_count = 0;
                let mut ignored_count = 0;
                writeln!(buf, "running {} tests", case_status.len()).unwrap();
                for (case_idx, case_result) in case_status.iter() {
                    let status = match case_result {
                        TestCaseStatus::Succeeded { .. } => {
                            success_count += 1;
                            global_success_count += 1;
                            Color::Green.paint("ok")
                        }
                        TestCaseStatus::Failed { reason } => {
                            failure_count += 1;
                            global_failure_count += 1;
                            Color::Red.paint(format!("FAILED, {reason}"))
                        }
                        TestCaseStatus::Ignored { reason, .. } => {
                            ignored_count += 1;
                            global_ignore_count += 1;
                            Color::Yellow.paint(format!("ignored, {reason:?}"))
                        }
                    };
                    writeln!(buf, "test case_idx_{} ... {}", case_idx, status).unwrap();
                }
                writeln!(buf).unwrap();

                let status = if failure_count > 0 {
                    Color::Red.paint("FAILED")
                } else {
                    Color::Green.paint("ok")
                };
                writeln!(
                    buf,
                    "test result: {}. {} passed; {} failed; {} ignored",
                    status, success_count, failure_count, ignored_count,
                )
                .unwrap();
                writeln!(buf).unwrap();

                if aggregator_events_rx.is_empty() {
                    buf = tokio::task::spawn_blocking(move || {
                        buf.flush().unwrap();
                        buf
                    })
                    .await
                    .unwrap();
                }
            }
        }
    }
    info!("Aggregator Broadcast Channel Closed");

    // Summary at the end.
    match output_format {
        OutputFormat::Legacy => {
            writeln!(
                buf,
                "{} cases: {} cases succeeded, {} cases failed in {} seconds",
                global_success_count + global_failure_count + global_ignore_count,
                Color::Green.paint(global_success_count.to_string()),
                Color::Red.paint(global_failure_count.to_string()),
                start.elapsed().as_secs()
            )
            .unwrap();
        }
        OutputFormat::CargoTestLike => {
            writeln!(
                buf,
                "run finished. {} passed; {} failed; {} ignored; finished in {}s",
                global_success_count,
                global_failure_count,
                global_ignore_count,
                start.elapsed().as_secs()
            )
            .unwrap();
        }
    }
}
