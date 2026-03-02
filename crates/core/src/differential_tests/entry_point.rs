//! The main entry point into differential testing.

use std::{
    collections::BTreeMap,
    io::{BufWriter, Write, stderr},
    sync::Arc,
    time::Instant,
};

use ansi_term::{ANSIStrings, Color};
use anyhow::Context as _;
use futures::StreamExt;
use indexmap::IndexMap;
use revive_dt_common::types::PrivateKeyAllocator;
use revive_dt_config::{
    Context, FailFastConfiguration, OutputFormat, OutputFormatConfiguration, Test,
};
use revive_dt_core::Platform;
use revive_dt_format::corpus::Corpus;
use revive_dt_report::{Reporter, ReporterEvent, TestCaseStatus};
use tokio::sync::Mutex;
use tracing::{error, info, info_span, instrument};

use crate::{
    differential_tests::Driver,
    helpers::{
        CachedCompiler, CorpusDefinitionProcessor, NodePool, TestCaseIgnoreResolvedConfiguration,
        TestDefinition, create_test_definitions_stream, process_corpus,
    },
};

/// The number of test steps that were executed.
type StepsExecuted = usize;

/// State for test definition processing.
#[derive(Clone)]
struct TestDefinitionProcessorState {
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
}

/// The definition processor for tests.
struct TestDefinitionProcessor;

impl CorpusDefinitionProcessor for TestDefinitionProcessor {
    type Definition<'a> = TestDefinition<'a>;
    type ProcessResult = StepsExecuted;
    type State = TestDefinitionProcessorState;

    async fn process_definition<'a>(
        definition: &'a Self::Definition<'a>,
        cached_compiler: &'a CachedCompiler<'a>,
        state: Self::State,
    ) -> anyhow::Result<Self::ProcessResult> {
        Driver::new_root(definition, state.private_key_allocator, cached_compiler)
            .await?
            .execute_all()
            .await
    }

    fn on_success(
        definition: &Self::Definition<'_>,
        steps_executed: StepsExecuted,
    ) -> anyhow::Result<()> {
        definition
            .reporter
            .report_test_succeeded_event(steps_executed)?;
        Ok(())
    }

    fn on_failure(definition: &Self::Definition<'_>, error: String) -> anyhow::Result<()> {
        definition.reporter.report_test_failed_event(error)?;
        Ok(())
    }

    fn on_ignored(definition: &Self::Definition<'_>, reason: String) -> anyhow::Result<()> {
        definition
            .reporter
            .report_test_ignored_event(reason, IndexMap::new())?;
        Ok(())
    }

    fn create_fail_fast_action(
        definition: &Self::Definition<'_>,
        fail_fast: &FailFastConfiguration,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        fail_fast.fail_fast.then(|| {
            let reporter = definition.reporter.clone();
            Box::new(move || {
                let _ = reporter.report_test_ignored_event(
                    "Aborted due to fail-fast".to_string(),
                    IndexMap::new(),
                );
            }) as Box<dyn FnOnce() + Send>
        })
    }

    fn create_span(task_id: usize, definition: &Self::Definition<'_>) -> tracing::Span {
        info_span!(
            "Executing Test Case",
            test_id = task_id,
            metadata_file_path = %definition.metadata_file_path.display(),
            case_idx = %definition.case_idx,
            mode = %definition.mode,
        )
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
    .context("Failed to initialize cached compiler")?;

    let state = TestDefinitionProcessorState {
        private_key_allocator: Arc::new(Mutex::new(PrivateKeyAllocator::new(
            context.wallet.highest_private_key_exclusive(),
        ))),
    };

    let cli_reporting_task =
        tokio::spawn(start_cli_reporting_task(context.output_format, reporter));

    process_corpus::<TestDefinitionProcessor>(
        &test_definitions,
        &cached_compiler,
        state,
        &context.concurrency,
        &context.fail_fast,
        reporter_clone,
    )
    .await;

    cli_reporting_task
        .await
        .expect("CLI reporting task panicked");

    Ok(())
}

#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(output_format: OutputFormatConfiguration, reporter: Reporter) {
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

        match output_format.output_format {
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
    match output_format.output_format {
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
