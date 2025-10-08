//! The main entry point into differential testing.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufWriter, Write, stderr},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::Platform;
use anyhow::Context as _;
use futures::{FutureExt, StreamExt};
use revive_dt_common::types::PrivateKeyAllocator;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tracing::{Instrument, error, info, info_span, instrument};

use revive_dt_config::{Context, TestExecutionContext};
use revive_dt_report::{Reporter, ReporterEvent, TestCaseStatus};

use crate::{
    differential_tests::Driver,
    helpers::{CachedCompiler, NodePool, collect_metadata_files, create_test_definitions_stream},
};

/// Handles the differential testing executing it according to the information defined in the
/// context
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_differential_tests(
    context: TestExecutionContext,
    reporter: Reporter,
) -> anyhow::Result<()> {
    let reporter_clone = reporter.clone();

    // Discover all of the metadata files that are defined in the context.
    let metadata_files = collect_metadata_files(&context)
        .context("Failed to collect metadata files for differential testing")?;
    info!(len = metadata_files.len(), "Discovered metadata files");

    // Discover the list of platforms that the tests should run on based on the context.
    let platforms =
        context.platforms.iter().copied().map(Into::<&dyn Platform>::into).collect::<Vec<_>>();

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
    let full_context = Context::Test(Box::new(context.clone()));
    let test_definitions = create_test_definitions_stream(
        &full_context,
        metadata_files.iter(),
        &platforms_and_nodes,
        reporter.clone(),
    )
    .await
    .collect::<Vec<_>>()
    .await;
    info!(len = test_definitions.len(), "Created test definitions");

    // Creating everything else required for the driver to run.
    let cached_compiler = CachedCompiler::new(
        context.working_directory.as_path().join("compilation_cache"),
        context.compilation_configuration.invalidate_compilation_cache,
    )
    .await
    .map(Arc::new)
    .context("Failed to initialize cached compiler")?;
    let private_key_allocator = Arc::new(Mutex::new(PrivateKeyAllocator::new(
        context.wallet_configuration.highest_private_key_exclusive(),
    )));

    // Creating the driver and executing all of the steps.
    let semaphore =
        context.concurrency_configuration.concurrency_limit().map(Semaphore::new).map(Arc::new);
    let running_task_list = Arc::new(RwLock::new(BTreeSet::<usize>::new()));
    let driver_task = futures::future::join_all(test_definitions.iter().enumerate().map(
        |(test_id, test_definition)| {
            let running_task_list = running_task_list.clone();
            let semaphore = semaphore.clone();

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
                let permit = match semaphore.as_ref() {
                    Some(semaphore) => Some(semaphore.acquire().await.expect("Can't fail")),
                    None => None,
                };

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
                        error!("Test Case Failed");
                    }
                };
                info!("Finished the execution of the test case");
                drop(permit);
                running_task_list.write().await.remove(&test_id);
            }
            .instrument(span)
        },
    ))
    .inspect(|_| {
        info!("Finished executing all test cases");
        reporter_clone.report_completion_event().expect("Can't fail")
    });
    let cli_reporting_task = start_cli_reporting_task(reporter);

    tokio::task::spawn(async move {
        loop {
            let remaining_tasks = running_task_list.read().await;
            info!(count = remaining_tasks.len(), ?remaining_tasks, "Remaining Tests");
            tokio::time::sleep(Duration::from_secs(10)).await
        }
    });

    futures::future::join(driver_task, cli_reporting_task).await;

    Ok(())
}

#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(reporter: Reporter) {
    let mut aggregator_events_rx = reporter.subscribe().await.expect("Can't fail");
    drop(reporter);

    let start = Instant::now();

    const GREEN: &str = "\x1B[32m";
    const RED: &str = "\x1B[31m";
    const GREY: &str = "\x1B[90m";
    const COLOR_RESET: &str = "\x1B[0m";
    const BOLD: &str = "\x1B[1m";
    const BOLD_RESET: &str = "\x1B[22m";

    let mut number_of_successes = 0;
    let mut number_of_failures = 0;

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

        let _ = writeln!(buf, "{} - {}", mode, metadata_file_path.display());
        for (case_idx, case_status) in case_status.into_iter() {
            let _ = write!(buf, "\tCase Index {case_idx:>3}: ");
            let _ = match case_status {
                TestCaseStatus::Succeeded {
                    steps_executed,
                } => {
                    number_of_successes += 1;
                    writeln!(
                        buf,
                        "{}{}Case Succeeded{} - Steps Executed: {}{}",
                        GREEN, BOLD, BOLD_RESET, steps_executed, COLOR_RESET
                    )
                }
                TestCaseStatus::Failed {
                    reason,
                } => {
                    number_of_failures += 1;
                    writeln!(
                        buf,
                        "{}{}Case Failed{} - Reason: {}{}",
                        RED,
                        BOLD,
                        BOLD_RESET,
                        reason.trim(),
                        COLOR_RESET,
                    )
                }
                TestCaseStatus::Ignored {
                    reason,
                    ..
                } => writeln!(
                    buf,
                    "{}{}Case Ignored{} - Reason: {}{}",
                    GREY,
                    BOLD,
                    BOLD_RESET,
                    reason.trim(),
                    COLOR_RESET,
                ),
            };
        }
        let _ = writeln!(buf);
    }

    // Summary at the end.
    let _ = writeln!(
        buf,
        "{} cases: {}{}{} cases succeeded, {}{}{} cases failed in {} seconds",
        number_of_successes + number_of_failures,
        GREEN,
        number_of_successes,
        COLOR_RESET,
        RED,
        number_of_failures,
        COLOR_RESET,
        start.elapsed().as_secs()
    );
}
