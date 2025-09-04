mod cached_compiler;

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
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
    NodeDesignation, ReportAggregator, Reporter, ReporterEvent, TestCaseStatus,
    TestSpecificReporter, TestSpecifier,
};
use serde_json::{Value, json};
use tokio::try_join;
use tracing::{debug, error, info, info_span, instrument};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use revive_dt_common::{iterators::EitherIter, types::Mode};
use revive_dt_compiler::{CompilerOutput, SolidityCompiler};
use revive_dt_config::{Context, *};
use revive_dt_core::{
    Geth, Kitchensink, Platform,
    driver::{CaseDriver, CaseState},
};
use revive_dt_format::{
    case::{Case, CaseIdx},
    corpus::Corpus,
    input::{Input, Step},
    metadata::{ContractPathAndIdent, MetadataFile},
    mode::ParsedMode,
};
use revive_dt_node::{Node, pool::NodePool};

use crate::cached_compiler::CachedCompiler;

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
                    execute_corpus(context, &tests, reporter, report_aggregator_task)
                        .await
                        .context("Failed to execute corpus")
                })
        }
    }
}

#[instrument(level = "debug", name = "Collecting Corpora", skip_all)]
fn collect_corpora(
    context: &ExecutionContext,
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

async fn run_driver<L, F>(
    context: ExecutionContext,
    metadata_files: &[MetadataFile],
    reporter: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_nodes = NodePool::<L::Blockchain>::new(context.clone())
        .context("Failed to initialize leader node pool")?;
    let follower_nodes = NodePool::<F::Blockchain>::new(context.clone())
        .context("Failed to initialize follower node pool")?;

    let tests_stream = tests_stream(
        &context,
        metadata_files.iter(),
        &leader_nodes,
        &follower_nodes,
        reporter.clone(),
    )
    .await;
    let driver_task = start_driver_task::<L, F>(&context, tests_stream)
        .await
        .context("Failed to start driver task")?;
    let cli_reporting_task = start_cli_reporting_task(reporter);

    let (_, _, rtn) = tokio::join!(cli_reporting_task, driver_task, report_aggregator_task);
    rtn?;

    Ok(())
}

async fn tests_stream<'a, L, F>(
    args: &ExecutionContext,
    metadata_files: impl IntoIterator<Item = &'a MetadataFile> + Clone,
    leader_node_pool: &'a NodePool<L::Blockchain>,
    follower_node_pool: &'a NodePool<F::Blockchain>,
    reporter: Reporter,
) -> impl Stream<Item = Test<'a, L, F>>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
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
                let leader_compiler = <L::Compiler as SolidityCompiler>::new(
                    args,
                    mode.version.clone().map(Into::into),
                )
                .await
                .inspect_err(|err| error!(?err, "Failed to instantiate the leader compiler"))
                .ok()?;

                let follower_compiler = <F::Compiler as SolidityCompiler>::new(
                    args,
                    mode.version.clone().map(Into::into),
                )
                .await
                .inspect_err(|err| error!(?err, "Failed to instantiate the follower compiler"))
                .ok()?;

                let leader_node = leader_node_pool.round_robbin();
                let follower_node = follower_node_pool.round_robbin();

                Some(Test::<L, F> {
                    metadata: metadata_file,
                    metadata_file_path: metadata_file.metadata_file_path.as_path(),
                    mode: mode.clone(),
                    case_idx: CaseIdx::new(case_idx),
                    case,
                    leader_node,
                    follower_node,
                    leader_compiler,
                    follower_compiler,
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

async fn start_driver_task<'a, L, F>(
    context: &ExecutionContext,
    tests: impl Stream<Item = Test<'a, L, F>>,
) -> anyhow::Result<impl Future<Output = ()>>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    L::Compiler: 'a,
    F::Compiler: 'a,
{
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
                test.reporter
                    .report_leader_node_assigned_event(
                        test.leader_node.id(),
                        *L::config_id(),
                        test.leader_node.connection_string(),
                    )
                    .expect("Can't fail");
                test.reporter
                    .report_follower_node_assigned_event(
                        test.follower_node.id(),
                        *F::config_id(),
                        test.follower_node.connection_string(),
                    )
                    .expect("Can't fail");

                let reporter = test.reporter.clone();
                let result = handle_case_driver::<L, F>(test, cached_compiler).await;

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
        leader_node = test.leader_node.id(),
        follower_node = test.follower_node.id(),
    )
)]
async fn handle_case_driver<'a, L, F>(
    test: Test<'a, L, F>,
    cached_compiler: Arc<CachedCompiler<'a>>,
) -> anyhow::Result<usize>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    L::Compiler: 'a,
    F::Compiler: 'a,
{
    let leader_reporter = test
        .reporter
        .execution_specific_reporter(test.leader_node.id(), NodeDesignation::Leader);
    let follower_reporter = test
        .reporter
        .execution_specific_reporter(test.follower_node.id(), NodeDesignation::Follower);

    let (
        CompilerOutput {
            contracts: leader_pre_link_contracts,
        },
        CompilerOutput {
            contracts: follower_pre_link_contracts,
        },
    ) = try_join!(
        cached_compiler.compile_contracts::<L>(
            test.metadata,
            test.metadata_file_path,
            test.mode.clone(),
            None,
            &test.leader_compiler,
            &leader_reporter,
        ),
        cached_compiler.compile_contracts::<F>(
            test.metadata,
            test.metadata_file_path,
            test.mode.clone(),
            None,
            &test.follower_compiler,
            &follower_reporter
        )
    )
    .context("Failed to compile pre-link contracts for leader/follower in parallel")?;

    let mut leader_deployed_libraries = None::<HashMap<_, _>>;
    let mut follower_deployed_libraries = None::<HashMap<_, _>>;
    let mut contract_sources = test
        .metadata
        .contract_sources()
        .context("Failed to retrieve contract sources from metadata")?;
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
        } = contract_sources
            .remove(library_instance)
            .context("Failed to find the contract source")?;

        let (leader_code, leader_abi) = leader_pre_link_contracts
            .get(&library_source_path)
            .and_then(|contracts| contracts.get(library_ident.as_str()))
            .context("Declared library was not compiled")?;
        let (follower_code, follower_abi) = follower_pre_link_contracts
            .get(&library_source_path)
            .and_then(|contracts| contracts.get(library_ident.as_str()))
            .context("Declared library was not compiled")?;

        let leader_code = match alloy::hex::decode(leader_code) {
            Ok(code) => code,
            Err(error) => {
                anyhow::bail!("Failed to hex-decode the byte code {}", error)
            }
        };
        let follower_code = match alloy::hex::decode(follower_code) {
            Ok(code) => code,
            Err(error) => {
                anyhow::bail!("Failed to hex-decode the byte code {}", error)
            }
        };

        // Getting the deployer address from the cases themselves. This is to ensure that we're
        // doing the deployments from different accounts and therefore we're not slowed down by
        // the nonce.
        let deployer_address = test
            .case
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::FunctionCall(input) => Some(input.caller),
                Step::BalanceAssertion(..) => None,
                Step::StorageEmptyAssertion(..) => None,
            })
            .next()
            .unwrap_or(Input::default_caller());
        let leader_tx = TransactionBuilder::<Ethereum>::with_deploy_code(
            TransactionRequest::default().from(deployer_address),
            leader_code,
        );
        let follower_tx = TransactionBuilder::<Ethereum>::with_deploy_code(
            TransactionRequest::default().from(deployer_address),
            follower_code,
        );

        let (leader_receipt, follower_receipt) = try_join!(
            test.leader_node.execute_transaction(leader_tx),
            test.follower_node.execute_transaction(follower_tx)
        )?;

        debug!(
            ?library_instance,
            library_address = ?leader_receipt.contract_address,
            "Deployed library to leader"
        );
        debug!(
            ?library_instance,
            library_address = ?follower_receipt.contract_address,
            "Deployed library to follower"
        );

        let leader_library_address = leader_receipt
            .contract_address
            .context("Contract deployment didn't return an address")?;
        let follower_library_address = follower_receipt
            .contract_address
            .context("Contract deployment didn't return an address")?;

        leader_deployed_libraries.get_or_insert_default().insert(
            library_instance.clone(),
            (
                library_ident.clone(),
                leader_library_address,
                leader_abi.clone(),
            ),
        );
        follower_deployed_libraries.get_or_insert_default().insert(
            library_instance.clone(),
            (
                library_ident,
                follower_library_address,
                follower_abi.clone(),
            ),
        );
    }
    if let Some(ref leader_deployed_libraries) = leader_deployed_libraries {
        leader_reporter.report_libraries_deployed_event(
            leader_deployed_libraries
                .clone()
                .into_iter()
                .map(|(key, (_, address, _))| (key, address))
                .collect::<BTreeMap<_, _>>(),
        )?;
    }
    if let Some(ref follower_deployed_libraries) = follower_deployed_libraries {
        follower_reporter.report_libraries_deployed_event(
            follower_deployed_libraries
                .clone()
                .into_iter()
                .map(|(key, (_, address, _))| (key, address))
                .collect::<BTreeMap<_, _>>(),
        )?;
    }

    let (
        CompilerOutput {
            contracts: leader_post_link_contracts,
        },
        CompilerOutput {
            contracts: follower_post_link_contracts,
        },
    ) = try_join!(
        cached_compiler.compile_contracts::<L>(
            test.metadata,
            test.metadata_file_path,
            test.mode.clone(),
            leader_deployed_libraries.as_ref(),
            &test.leader_compiler,
            &leader_reporter,
        ),
        cached_compiler.compile_contracts::<F>(
            test.metadata,
            test.metadata_file_path,
            test.mode.clone(),
            follower_deployed_libraries.as_ref(),
            &test.follower_compiler,
            &follower_reporter
        )
    )
    .context("Failed to compile post-link contracts for leader/follower in parallel")?;

    let leader_state = CaseState::<L>::new(
        test.leader_compiler.version().clone(),
        leader_post_link_contracts,
        leader_deployed_libraries.unwrap_or_default(),
        leader_reporter,
    );
    let follower_state = CaseState::<F>::new(
        test.follower_compiler.version().clone(),
        follower_post_link_contracts,
        follower_deployed_libraries.unwrap_or_default(),
        follower_reporter,
    );

    let mut driver = CaseDriver::<L, F>::new(
        test.metadata,
        test.case,
        test.leader_node,
        test.follower_node,
        leader_state,
        follower_state,
    );
    driver
        .execute()
        .await
        .inspect(|steps_executed| info!(steps_executed, "Case succeeded"))
}

async fn execute_corpus(
    context: ExecutionContext,
    tests: &[MetadataFile],
    reporter: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    match (&context.leader, &context.follower) {
        (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
            run_driver::<Geth, Kitchensink>(context, tests, reporter, report_aggregator_task)
                .await?
        }
        (TestingPlatform::Geth, TestingPlatform::Geth) => {
            run_driver::<Geth, Geth>(context, tests, reporter, report_aggregator_task).await?
        }
        _ => unimplemented!(),
    }

    Ok(())
}

/// this represents a single "test"; a mode, path and collection of cases.
#[derive(Clone)]
struct Test<'a, L: Platform, F: Platform> {
    metadata: &'a MetadataFile,
    metadata_file_path: &'a Path,
    mode: Cow<'a, Mode>,
    case_idx: CaseIdx,
    case: &'a Case,
    leader_node: &'a <L as Platform>::Blockchain,
    follower_node: &'a <F as Platform>::Blockchain,
    leader_compiler: L::Compiler,
    follower_compiler: F::Compiler,
    reporter: TestSpecificReporter,
}

impl<'a, L: Platform, F: Platform> Test<'a, L, F> {
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

    /// Checks if the leader and the follower both support the desired targets in the metadata file.
    fn check_target_compatibility(&self) -> TestCheckFunctionResult {
        let leader_support =
            <L::Blockchain as Node>::matches_target(self.metadata.targets.as_deref());
        let follower_support =
            <F::Blockchain as Node>::matches_target(self.metadata.targets.as_deref());
        let is_allowed = leader_support && follower_support;

        if is_allowed {
            Ok(())
        } else {
            Err((
                "Either the leader or the follower do not support the target desired by the test.",
                indexmap! {
                    "test_desired_targets" => json!(self.metadata.targets.as_ref()),
                    "leader_support" => json!(leader_support),
                    "follower_support" => json!(follower_support),
                },
            ))
        }
    }

    // Checks for the compatibility of the EVM version with the leader and follower nodes.
    fn check_evm_version_compatibility(&self) -> TestCheckFunctionResult {
        let Some(evm_version_requirement) = self.metadata.required_evm_version else {
            return Ok(());
        };

        let leader_support = evm_version_requirement
            .matches(&<L::Blockchain as revive_dt_node::Node>::evm_version());
        let follower_support = evm_version_requirement
            .matches(&<F::Blockchain as revive_dt_node::Node>::evm_version());
        let is_allowed = leader_support && follower_support;

        if is_allowed {
            Ok(())
        } else {
            Err((
                "EVM version is incompatible with either the leader or the follower.",
                indexmap! {
                    "test_desired_evm_version" => json!(self.metadata.required_evm_version),
                    "leader_support" => json!(leader_support),
                    "follower_support" => json!(follower_support),
                },
            ))
        }
    }

    /// Checks if the leader and follower compilers support the mode that the test is for.
    fn check_compiler_compatibility(&self) -> TestCheckFunctionResult {
        let leader_support = self
            .leader_compiler
            .supports_mode(self.mode.optimize_setting, self.mode.pipeline);
        let follower_support = self
            .follower_compiler
            .supports_mode(self.mode.optimize_setting, self.mode.pipeline);
        let is_allowed = leader_support && follower_support;

        if is_allowed {
            Ok(())
        } else {
            Err((
                "Compilers do not support this mode either for the leader or for the follower.",
                indexmap! {
                    "mode" => json!(self.mode),
                    "leader_support" => json!(leader_support),
                    "follower_support" => json!(follower_support),
                },
            ))
        }
    }
}

type TestCheckFunctionResult = Result<(), (&'static str, IndexMap<&'static str, Value>)>;
