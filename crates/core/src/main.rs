mod cached_compiler;
mod pool;

use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    io::{BufWriter, Write, stderr},
    path::Path,
    sync::Arc,
    time::Instant,
};

use alloy::{
    network::{Ethereum, TransactionBuilder},
    rpc::types::TransactionRequest,
};
use anyhow::Context as _;
use clap::Parser;
use futures::stream;
use futures::{Stream, StreamExt};
use indexmap::{IndexMap, indexmap};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::{
    ExecutionSpecificReporter, ReportAggregator, Reporter, ReporterEvent, TestCaseStatus,
    TestSpecificReporter, TestSpecifier,
};
use schemars::schema_for;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{debug, error, info, info_span, instrument};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use revive_dt_common::{
    iterators::EitherIter,
    types::{Mode, PrivateKeyAllocator},
};
use revive_dt_compiler::SolidityCompiler;
use revive_dt_config::{Context, *};
use revive_dt_core::{
    Platform,
    driver::{CaseDriver, CaseState},
};
use revive_dt_format::{
    case::{Case, CaseIdx},
    corpus::Corpus,
    metadata::{ContractPathAndIdent, Metadata, MetadataFile},
    mode::ParsedMode,
    steps::{FunctionCallStep, Step},
};

use crate::cached_compiler::CachedCompiler;
use crate::pool::NodePool;

fn main() -> anyhow::Result<()> {
    let (writer, _guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        // Assuming that each line contains 255 characters and that each character is one byte, then
        // this means that our buffer is about 4GBs large.
        .buffered_lines_limit(0x1000000)
        .thread_name("buffered writer")
        .finish(std::io::stdout());

    let subscriber = FmtSubscriber::builder()
        .with_writer(writer)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .pretty()
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    info!("Differential testing tool is starting");

    let context = Context::try_parse()?;
    let (reporter, report_aggregator_task) = ReportAggregator::new(context.clone()).into_task();

    match context {
        Context::ExecuteTests(context) => {
            let tests = collect_corpora(&context)
                .context("Failed to collect corpus files from provided arguments")?
                .into_iter()
                .inspect(|(corpus, _)| {
                    reporter
                        .report_corpus_file_discovery_event(corpus.clone())
                        .expect("Can't fail")
                })
                .flat_map(|(_, files)| files.into_iter())
                .inspect(|metadata_file| {
                    reporter
                        .report_metadata_file_discovery_event(
                            metadata_file.metadata_file_path.clone(),
                            metadata_file.content.clone(),
                        )
                        .expect("Can't fail")
                })
                .collect::<Vec<_>>();

            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(context.concurrency_configuration.number_of_threads)
                .enable_all()
                .build()
                .expect("Failed building the Runtime")
                .block_on(async move {
                    execute_corpus(*context, &tests, reporter, report_aggregator_task)
                        .await
                        .context("Failed to execute corpus")
                })
        }
        Context::ExportJsonSchema => {
            let schema = schema_for!(Metadata);
            println!("{}", serde_json::to_string_pretty(&schema).unwrap());
            Ok(())
        }
    }
}

#[instrument(level = "debug", name = "Collecting Corpora", skip_all)]
fn collect_corpora(
    context: &TestExecutionContext,
) -> anyhow::Result<HashMap<Corpus, Vec<MetadataFile>>> {
    let mut corpora = HashMap::new();

    for path in &context.corpus {
        let span = info_span!("Processing corpus file", path = %path.display());
        let _guard = span.enter();

        let corpus = Corpus::try_from_path(path)?;
        info!(
            name = corpus.name(),
            number_of_contained_paths = corpus.path_count(),
            "Deserialized corpus file"
        );
        let tests = corpus.enumerate_tests();
        corpora.insert(corpus, tests);
    }

    Ok(corpora)
}

async fn run_driver(
    context: TestExecutionContext,
    metadata_files: &[MetadataFile],
    reporter: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
    platforms: Vec<&dyn Platform>,
) -> anyhow::Result<()> {
    let mut nodes = Vec::<(&dyn Platform, NodePool)>::new();
    for platform in platforms.into_iter() {
        let pool = NodePool::new(Context::ExecuteTests(Box::new(context.clone())), platform)
            .inspect_err(|err| {
                error!(
                    ?err,
                    platform_identifier = %platform.platform_identifier(),
                    "Failed to initialize the node pool for the platform."
                )
            })
            .context("Failed to initialize the node pool")?;
        nodes.push((platform, pool));
    }

    let tests_stream = tests_stream(
        &context,
        metadata_files.iter(),
        nodes.as_slice(),
        reporter.clone(),
    )
    .await;
    let driver_task = start_driver_task(&context, tests_stream)
        .await
        .context("Failed to start driver task")?;
    let cli_reporting_task = start_cli_reporting_task(reporter);

    let (_, _, rtn) = tokio::join!(cli_reporting_task, driver_task, report_aggregator_task);
    rtn?;

    Ok(())
}

async fn tests_stream<'a>(
    args: &TestExecutionContext,
    metadata_files: impl IntoIterator<Item = &'a MetadataFile> + Clone,
    nodes: &'a [(&dyn Platform, NodePool)],
    reporter: Reporter,
) -> impl Stream<Item = Test<'a>> {
    let tests = metadata_files
        .into_iter()
        .flat_map(|metadata_file| {
            metadata_file
                .cases
                .iter()
                .enumerate()
                .map(move |(case_idx, case)| (metadata_file, case_idx, case))
        })
        // Flatten over the modes, prefer the case modes over the metadata file modes.
        .flat_map(|(metadata_file, case_idx, case)| {
            let reporter = reporter.clone();

            let modes = case.modes.as_ref().or(metadata_file.modes.as_ref());
            let modes = match modes {
                Some(modes) => EitherIter::A(
                    ParsedMode::many_to_modes(modes.iter()).map(Cow::<'static, _>::Owned),
                ),
                None => EitherIter::B(Mode::all().map(Cow::<'static, _>::Borrowed)),
            };

            modes.into_iter().map(move |mode| {
                (
                    metadata_file,
                    case_idx,
                    case,
                    mode.clone(),
                    reporter.test_specific_reporter(Arc::new(TestSpecifier {
                        solc_mode: mode.as_ref().clone(),
                        metadata_file_path: metadata_file.metadata_file_path.clone(),
                        case_idx: CaseIdx::new(case_idx),
                    })),
                )
            })
        })
        .collect::<Vec<_>>();

    // Note: before we do any kind of filtering or process the iterator in any way, we need to
    // inform the report aggregator of all of the cases that were found as it keeps a state of the
    // test cases for its internal use.
    for (_, _, _, _, reporter) in tests.iter() {
        reporter
            .report_test_case_discovery_event()
            .expect("Can't fail")
    }

    stream::iter(tests.into_iter())
        .filter_map(
            move |(metadata_file, case_idx, case, mode, reporter)| async move {
                let mut platforms = Vec::new();
                for (platform, node_pool) in nodes.iter() {
                    let node = node_pool.round_robbin();
                    let compiler = platform
                        .new_compiler(
                            Context::ExecuteTests(Box::new(args.clone())),
                            mode.version.clone().map(Into::into),
                        )
                        .await
                        .inspect_err(|err| {
                            error!(
                                ?err,
                                platform_identifier = %platform.platform_identifier(),
                                "Failed to instantiate the compiler"
                            )
                        })
                        .ok()?;

                    let reporter = reporter
                        .execution_specific_reporter(node.id(), platform.platform_identifier());
                    platforms.push((*platform, node, compiler, reporter));
                }

                Some(Test {
                    metadata: metadata_file,
                    metadata_file_path: metadata_file.metadata_file_path.as_path(),
                    mode: mode.clone(),
                    case_idx: CaseIdx::new(case_idx),
                    case,
                    platforms,
                    reporter,
                })
            },
        )
        .filter_map(move |test| async move {
            match test.check_compatibility() {
                Ok(()) => Some(test),
                Err((reason, additional_information)) => {
                    debug!(
                        metadata_file_path = %test.metadata.metadata_file_path.display(),
                        case_idx = %test.case_idx,
                        mode = %test.mode,
                        reason,
                        additional_information =
                            serde_json::to_string(&additional_information).unwrap(),

                        "Ignoring Test Case"
                    );
                    test.reporter
                        .report_test_ignored_event(
                            reason.to_string(),
                            additional_information
                                .into_iter()
                                .map(|(k, v)| (k.into(), v))
                                .collect::<IndexMap<_, _>>(),
                        )
                        .expect("Can't fail");
                    None
                }
            }
        })
}

async fn start_driver_task<'a>(
    context: &TestExecutionContext,
    tests: impl Stream<Item = Test<'a>>,
) -> anyhow::Result<impl Future<Output = ()>> {
    info!("Starting driver task");

    let cached_compiler = Arc::new(
        CachedCompiler::new(
            context
                .working_directory
                .as_path()
                .join("compilation_cache"),
            context
                .compilation_configuration
                .invalidate_compilation_cache,
        )
        .await
        .context("Failed to initialize cached compiler")?,
    );

    Ok(tests.for_each_concurrent(
        context.concurrency_configuration.concurrency_limit(),
        move |test| {
            let cached_compiler = cached_compiler.clone();

            async move {
                for (platform, node, _, _) in test.platforms.iter() {
                    test.reporter
                        .report_node_assigned_event(
                            node.id(),
                            platform.platform_identifier(),
                            node.connection_string(),
                        )
                        .expect("Can't fail");
                }

                let private_key_allocator = Arc::new(Mutex::new(PrivateKeyAllocator::new(
                    context.wallet_configuration.highest_private_key_exclusive(),
                )));

                let reporter = test.reporter.clone();
                let result =
                    handle_case_driver(&test, cached_compiler, private_key_allocator).await;

                match result {
                    Ok(steps_executed) => reporter
                        .report_test_succeeded_event(steps_executed)
                        .expect("Can't fail"),
                    Err(error) => reporter
                        .report_test_failed_event(format!("{error:#}"))
                        .expect("Can't fail"),
                }
            }
        },
    ))
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
                TestCaseStatus::Succeeded { steps_executed } => {
                    number_of_successes += 1;
                    writeln!(
                        buf,
                        "{}{}Case Succeeded{} - Steps Executed: {}{}",
                        GREEN, BOLD, BOLD_RESET, steps_executed, COLOR_RESET
                    )
                }
                TestCaseStatus::Failed { reason } => {
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
                TestCaseStatus::Ignored { reason, .. } => writeln!(
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

#[allow(clippy::too_many_arguments)]
#[instrument(
    level = "info",
    name = "Handling Case"
    skip_all,
    fields(
        metadata_file_path = %test.metadata.relative_path().display(),
        mode = %test.mode,
        case_idx = %test.case_idx,
        case_name = test.case.name.as_deref().unwrap_or("Unnamed Case"),
    )
)]
async fn handle_case_driver<'a>(
    test: &Test<'a>,
    cached_compiler: Arc<CachedCompiler<'a>>,
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
) -> anyhow::Result<usize> {
    let platform_state = stream::iter(test.platforms.iter())
        // Compiling the pre-link contracts.
        .filter_map(|(platform, node, compiler, reporter)| {
            let cached_compiler = cached_compiler.clone();

            async move {
                let compiler_output = cached_compiler
                    .compile_contracts(
                        test.metadata,
                        test.metadata_file_path,
                        test.mode.clone(),
                        None,
                        compiler.as_ref(),
                        *platform,
                        reporter,
                    )
                    .await
                    .inspect_err(|err| {
                        error!(
                            ?err,
                            platform_identifier = %platform.platform_identifier(),
                            "Pre-linking compilation failed"
                        )
                    })
                    .ok()?;
                Some((test, platform, node, compiler, reporter, compiler_output))
            }
        })
        // Deploying the libraries for the platform.
        .filter_map(
            |(test, platform, node, compiler, reporter, compiler_output)| async move {
                let mut deployed_libraries = None::<HashMap<_, _>>;
                let mut contract_sources = test
                    .metadata
                    .contract_sources()
                    .inspect_err(|err| {
                        error!(
                            ?err,
                            platform_identifier = %platform.platform_identifier(),
                            "Failed to retrieve contract sources from metadata"
                        )
                    })
                    .ok()?;
                for library_instance in test
                    .metadata
                    .libraries
                    .iter()
                    .flatten()
                    .flat_map(|(_, map)| map.values())
                {
                    debug!(%library_instance, "Deploying Library Instance");

                    let ContractPathAndIdent {
                        contract_source_path: library_source_path,
                        contract_ident: library_ident,
                    } = contract_sources.remove(library_instance)?;

                    let (code, abi) = compiler_output
                        .contracts
                        .get(&library_source_path)
                        .and_then(|contracts| contracts.get(library_ident.as_str()))?;

                    let code = alloy::hex::decode(code).ok()?;

                    // Getting the deployer address from the cases themselves. This is to ensure
                    // that we're doing the deployments from different accounts and therefore we're
                    // not slowed down by the nonce.
                    let deployer_address = test
                        .case
                        .steps
                        .iter()
                        .filter_map(|step| match step {
                            Step::FunctionCall(input) => input.caller.as_address().copied(),
                            Step::BalanceAssertion(..) => None,
                            Step::StorageEmptyAssertion(..) => None,
                            Step::Repeat(..) => None,
                            Step::AllocateAccount(..) => None,
                        })
                        .next()
                        .unwrap_or(FunctionCallStep::default_caller_address());
                    let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                        TransactionRequest::default().from(deployer_address),
                        code,
                    );
                    let receipt = node
                        .execute_transaction(tx)
                        .await
                        .inspect_err(|err| {
                            error!(
                                ?err,
                                %library_instance,
                                platform_identifier = %platform.platform_identifier(),
                                "Failed to deploy the library"
                            )
                        })
                        .ok()?;

                    debug!(
                        ?library_instance,
                        platform_identifier = %platform.platform_identifier(),
                        "Deployed library"
                    );

                    let library_address = receipt.contract_address?;

                    deployed_libraries.get_or_insert_default().insert(
                        library_instance.clone(),
                        (library_ident.clone(), library_address, abi.clone()),
                    );
                }

                Some((
                    test,
                    platform,
                    node,
                    compiler,
                    reporter,
                    compiler_output,
                    deployed_libraries,
                ))
            },
        )
        // Compiling the post-link contracts.
        .filter_map(
            |(test, platform, node, compiler, reporter, _, deployed_libraries)| {
                let cached_compiler = cached_compiler.clone();
                let private_key_allocator = private_key_allocator.clone();

                async move {
                    let compiler_output = cached_compiler
                        .compile_contracts(
                            test.metadata,
                            test.metadata_file_path,
                            test.mode.clone(),
                            deployed_libraries.as_ref(),
                            compiler.as_ref(),
                            *platform,
                            reporter,
                        )
                        .await
                        .inspect_err(|err| {
                            error!(
                                ?err,
                                platform_identifier = %platform.platform_identifier(),
                                "Pre-linking compilation failed"
                            )
                        })
                        .ok()?;

                    let case_state = CaseState::new(
                        compiler.version().clone(),
                        compiler_output.contracts,
                        deployed_libraries.unwrap_or_default(),
                        reporter.clone(),
                        private_key_allocator,
                    );

                    Some((*node, platform.platform_identifier(), case_state))
                }
            },
        )
        // Collect
        .collect::<Vec<_>>()
        .await;

    let mut driver = CaseDriver::new(test.metadata, test.case, platform_state);
    driver
        .execute()
        .await
        .inspect(|steps_executed| info!(steps_executed, "Case succeeded"))
}

async fn execute_corpus(
    context: TestExecutionContext,
    tests: &[MetadataFile],
    reporter: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let platforms = context
        .platforms
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(Into::<&dyn Platform>::into)
        .collect::<Vec<_>>();

    run_driver(context, tests, reporter, report_aggregator_task, platforms).await?;

    Ok(())
}

/// this represents a single "test"; a mode, path and collection of cases.
#[allow(clippy::type_complexity)]
struct Test<'a> {
    metadata: &'a MetadataFile,
    metadata_file_path: &'a Path,
    mode: Cow<'a, Mode>,
    case_idx: CaseIdx,
    case: &'a Case,
    platforms: Vec<(
        &'a dyn Platform,
        &'a dyn EthereumNode,
        Box<dyn SolidityCompiler>,
        ExecutionSpecificReporter,
    )>,
    reporter: TestSpecificReporter,
}

impl<'a> Test<'a> {
    /// Checks if this test can be ran with the current configuration.
    pub fn check_compatibility(&self) -> TestCheckFunctionResult {
        self.check_metadata_file_ignored()?;
        self.check_case_file_ignored()?;
        self.check_target_compatibility()?;
        self.check_evm_version_compatibility()?;
        self.check_compiler_compatibility()?;
        Ok(())
    }

    /// Checks if the metadata file is ignored or not.
    fn check_metadata_file_ignored(&self) -> TestCheckFunctionResult {
        if self.metadata.ignore.is_some_and(|ignore| ignore) {
            Err(("Metadata file is ignored.", indexmap! {}))
        } else {
            Ok(())
        }
    }

    /// Checks if the case file is ignored or not.
    fn check_case_file_ignored(&self) -> TestCheckFunctionResult {
        if self.case.ignore.is_some_and(|ignore| ignore) {
            Err(("Case is ignored.", indexmap! {}))
        } else {
            Ok(())
        }
    }

    /// Checks if the platforms all support the desired targets in the metadata file.
    fn check_target_compatibility(&self) -> TestCheckFunctionResult {
        let mut error_map = indexmap! {
            "test_desired_targets" => json!(self.metadata.targets.as_ref()),
        };
        let mut is_allowed = true;
        for (platform, ..) in self.platforms.iter() {
            let is_allowed_for_platform = match self.metadata.targets.as_ref() {
                None => true,
                Some(targets) => {
                    let mut target_matches = false;
                    for target in targets.iter() {
                        if &platform.vm_identifier() == target {
                            target_matches = true;
                            break;
                        }
                    }
                    target_matches
                }
            };
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "One of the platforms do do not support the targets allowed by the test.",
                error_map,
            ))
        }
    }

    // Checks for the compatibility of the EVM version with the platforms specified.
    fn check_evm_version_compatibility(&self) -> TestCheckFunctionResult {
        let Some(evm_version_requirement) = self.metadata.required_evm_version else {
            return Ok(());
        };

        let mut error_map = indexmap! {
            "test_desired_evm_version" => json!(self.metadata.required_evm_version),
        };
        let mut is_allowed = true;
        for (platform, node, ..) in self.platforms.iter() {
            let is_allowed_for_platform = evm_version_requirement.matches(&node.evm_version());
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "EVM version is incompatible for the platforms specified",
                error_map,
            ))
        }
    }

    /// Checks if the platforms compilers support the mode that the test is for.
    fn check_compiler_compatibility(&self) -> TestCheckFunctionResult {
        let mut error_map = indexmap! {
            "test_desired_evm_version" => json!(self.metadata.required_evm_version),
        };
        let mut is_allowed = true;
        for (platform, _, compiler, ..) in self.platforms.iter() {
            let is_allowed_for_platform =
                compiler.supports_mode(self.mode.optimize_setting, self.mode.pipeline);
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "Compilers do not support this mode either for the provided platforms.",
                error_map,
            ))
        }
    }
}

type TestCheckFunctionResult = Result<(), (&'static str, IndexMap<&'static str, Value>)>;
