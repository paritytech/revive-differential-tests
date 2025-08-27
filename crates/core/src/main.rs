mod cached_compiler;

use std::{
    collections::{BTreeMap, HashMap},
    io::{BufWriter, Write, stderr},
    path::Path,
    sync::{Arc, LazyLock},
    time::Instant,
};

use alloy::{
    network::{Ethereum, TransactionBuilder},
    rpc::types::TransactionRequest,
};
use anyhow::Context;
use clap::Parser;
use futures::stream;
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::{
    NodeDesignation, ReportAggregator, Reporter, ReporterEvent, TestCaseStatus,
    TestSpecificReporter, TestSpecifier,
};
use temp_dir::TempDir;
use tokio::{join, try_join};
use tracing::{debug, info, info_span, instrument};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use revive_dt_common::types::Mode;
use revive_dt_compiler::{CompilerOutput, SolidityCompiler};
use revive_dt_config::*;
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

static TEMP_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

/// this represents a single "test"; a mode, path and collection of cases.
#[derive(Clone)]
struct Test<'a, L: Platform, F: Platform> {
    metadata: &'a MetadataFile,
    metadata_file_path: &'a Path,
    mode: Mode,
    case_idx: CaseIdx,
    case: &'a Case,
    leader_node: &'a <L as Platform>::Blockchain,
    follower_node: &'a <F as Platform>::Blockchain,
    reporter: TestSpecificReporter,
}

fn main() -> anyhow::Result<()> {
    let (args, _guard) = init_cli().context("Failed to initialize CLI and tracing subscriber")?;
    info!(
        leader = args.leader.to_string(),
        follower = args.follower.to_string(),
        working_directory = %args.directory().display(),
        number_of_nodes = args.number_of_nodes,
        invalidate_compilation_cache = args.invalidate_compilation_cache,
        "Differential testing tool has been initialized"
    );

    let (reporter, report_aggregator_task) = ReportAggregator::new(args.clone()).into_task();

    let number_of_threads = args.number_of_threads;
    let body = async move {
        let tests = collect_corpora(&args)
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

        match &args.compile_only {
            Some(platform) => {
                compile_corpus(&args, &tests, platform, reporter, report_aggregator_task).await
            }
            None => execute_corpus(&args, &tests, reporter, report_aggregator_task)
                .await
                .context("Failed to execute corpus")?,
        }
        Ok(())
    };

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(number_of_threads)
        .enable_all()
        .build()
        .expect("Failed building the Runtime")
        .block_on(body)
}

fn init_cli() -> anyhow::Result<(Arguments, WorkerGuard)> {
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
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

    let mut args = Arguments::parse();

    if args.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }

    match args.working_directory.as_ref() {
        Some(dir) => {
            if !dir.exists() {
                anyhow::bail!("workdir {} does not exist", dir.display());
            }
        }
        None => {
            args.temp_dir = Some(&TEMP_DIR);
        }
    }

    Ok((args, guard))
}

#[instrument(level = "debug", name = "Collecting Corpora", skip_all)]
fn collect_corpora(args: &Arguments) -> anyhow::Result<HashMap<Corpus, Vec<MetadataFile>>> {
    let mut corpora = HashMap::new();

    for path in &args.corpus {
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
    args: &Arguments,
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
    let leader_nodes =
        NodePool::<L::Blockchain>::new(args).context("Failed to initialize leader node pool")?;
    let follower_nodes =
        NodePool::<F::Blockchain>::new(args).context("Failed to initialize follower node pool")?;

    let tests = prepare_tests::<L, F>(
        args,
        metadata_files,
        &leader_nodes,
        &follower_nodes,
        reporter.clone(),
    );
    let driver_task = start_driver_task::<L, F>(args, tests)
        .await
        .context("Failed to start driver task")?;
    let cli_reporting_task = start_cli_reporting_task(reporter);

    let (_, _, rtn) = tokio::join!(cli_reporting_task, driver_task, report_aggregator_task);
    rtn?;

    Ok(())
}

fn prepare_tests<'a, L, F>(
    args: &Arguments,
    metadata_files: &'a [MetadataFile],
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
    let filtered_tests = metadata_files
        .iter()
        .flat_map(|metadata_file| {
            metadata_file
                .cases
                .iter()
                .enumerate()
                .map(move |(case_idx, case)| (metadata_file, case_idx, case))
        })
        // Flatten over the modes, prefer the case modes over the metadata file modes.
        .flat_map(|(metadata_file, case_idx, case)| {
            case.modes
                .as_ref()
                .or(metadata_file.modes.as_ref())
                .map(|modes| ParsedMode::many_to_modes(modes.iter()).collect::<Vec<_>>())
                .unwrap_or(Mode::all().collect())
                .into_iter()
                .map(move |mode| (metadata_file, case_idx, case, mode))
        })
        .map(move |(metadata_file, case_idx, case, mode)| {
            Test {
                metadata: metadata_file,
                metadata_file_path: metadata_file.metadata_file_path.as_path(),
                mode: mode.clone(),
                case_idx: CaseIdx::new(case_idx),
                case,
                leader_node: leader_node_pool.round_robbin(),
                follower_node: follower_node_pool.round_robbin(),
                reporter: reporter.test_specific_reporter(Arc::new(TestSpecifier {
                    solc_mode: mode.clone(),
                    metadata_file_path: metadata_file.metadata_file_path.clone(),
                    case_idx: CaseIdx::new(case_idx),
                })),
            }
        })
        .inspect(|test| {
            test.reporter
                .report_test_case_discovery_event()
                .expect("Can't fail")
        })
        .collect::<Vec<_>>()
        .into_iter()
        // Filter the test out if the leader and follower do not support the target.
        .filter(|test| {
            let leader_support =
                <L::Blockchain as Node>::matches_target(test.metadata.targets.as_deref());
            let follower_support =
                <F::Blockchain as Node>::matches_target(test.metadata.targets.as_deref());
            let is_allowed = leader_support && follower_support;

            if !is_allowed {
                debug!(
                    file_path = %test.metadata.relative_path().display(),
                    leader_support,
                    follower_support,
                    "Target is not supported, throwing metadata file out"
                );
                test
                    .reporter
                    .report_test_ignored_event(
                        "Either the leader or the follower do not support the target desired by the test",
                        IndexMap::from_iter([
                            (
                                "test_desired_targets".to_string(),
                                serde_json::to_value(test.metadata.targets.as_ref())
                                    .expect("Can't fail")
                            ),
                            (
                                "leader_support".to_string(),
                                serde_json::to_value(leader_support)
                                    .expect("Can't fail")
                            ),
                            (
                                "follower_support".to_string(),
                                serde_json::to_value(follower_support)
                                    .expect("Can't fail")
                            )
                        ])
                    )
                    .expect("Can't fail");
            }

            is_allowed
        })
        // Filter the test out if the metadata file is ignored.
        .filter(|test| {
            if test.metadata.ignore.is_some_and(|ignore| ignore) {
                debug!(
                    file_path = %test.metadata.relative_path().display(),
                    "Metadata file is ignored, throwing case out"
                );
                test
                    .reporter
                    .report_test_ignored_event(
                        "Metadata file is ignored, therefore all cases are ignored",
                        IndexMap::new(),
                    )
                    .expect("Can't fail");
                false
            } else {
                true
            }
        })
        // Filter the test case if the case is ignored.
        .filter(|test| {
            if test.case.ignore.is_some_and(|ignore| ignore) {
                debug!(
                    file_path = %test.metadata.relative_path().display(),
                    case_idx = %test.case_idx,
                    "Case is ignored, throwing case out"
                );
                test
                    .reporter
                    .report_test_ignored_event(
                        "Case is ignored",
                        IndexMap::new(),
                    )
                    .expect("Can't fail");
                false
            } else {
                true
            }
        })
        // Filtering based on the EVM version compatibility
        .filter(|test| {
            if let Some(evm_version_requirement) = test.metadata.required_evm_version {
                let leader_compatibility = evm_version_requirement
                    .matches(&<L::Blockchain as revive_dt_node::Node>::evm_version());
                let follower_compatibility = evm_version_requirement
                    .matches(&<F::Blockchain as revive_dt_node::Node>::evm_version());
                let is_allowed = leader_compatibility && follower_compatibility;

                if !is_allowed {
                    debug!(
                        file_path = %test.metadata.relative_path().display(),
                        case_idx = %test.case_idx,
                        leader_compatibility,
                        follower_compatibility,
                        "EVM Version is incompatible, throwing case out"
                    );
                    test
                        .reporter
                        .report_test_ignored_event(
                            "EVM version is incompatible with either the leader or the follower",
                            IndexMap::from_iter([
                                (
                                    "test_desired_evm_version".to_string(),
                                    serde_json::to_value(test.metadata.required_evm_version)
                                        .expect("Can't fail")
                                ),
                                (
                                    "leader_compatibility".to_string(),
                                    serde_json::to_value(leader_compatibility)
                                        .expect("Can't fail")
                                ),
                                (
                                    "follower_compatibility".to_string(),
                                    serde_json::to_value(follower_compatibility)
                                        .expect("Can't fail")
                                )
                            ])
                        )
                        .expect("Can't fail");
                }

                is_allowed
            } else {
                true
            }
        });

    stream::iter(filtered_tests)
        // Filter based on the compiler compatibility
        .filter_map(move |test| async move {
            let leader_support = does_compiler_support_mode::<L>(args, &test.mode)
                .await
                .ok()
                .unwrap_or(false);
            let follower_support = does_compiler_support_mode::<F>(args, &test.mode)
                .await
                .ok()
                .unwrap_or(false);
            let is_allowed = leader_support && follower_support;

            if !is_allowed {
                debug!(
                    file_path = %test.metadata.relative_path().display(),
                    leader_support,
                    follower_support,
                    "Compilers do not support this, throwing case out"
                );
                test
                    .reporter
                    .report_test_ignored_event(
                        "Compilers do not support this mode either for the leader or for the follower.",
                        IndexMap::from_iter([
                            (
                                "leader_support".to_string(),
                                serde_json::to_value(leader_support)
                                    .expect("Can't fail")
                            ),
                            (
                                "follower_support".to_string(),
                                serde_json::to_value(follower_support)
                                    .expect("Can't fail")
                            )
                        ])
                    )
                    .expect("Can't fail");
            }

            is_allowed.then_some(test)
        })
}

async fn does_compiler_support_mode<P: Platform>(
    args: &Arguments,
    mode: &Mode,
) -> anyhow::Result<bool> {
    let compiler_version_or_requirement = mode.compiler_version_to_use(args.solc.clone());
    let compiler_path = P::Compiler::get_compiler_executable(args, compiler_version_or_requirement)
        .await
        .context("Failed to obtain compiler executable path")?;
    let compiler_version = P::Compiler::new(compiler_path.clone())
        .version()
        .await
        .context("Failed to query compiler version")?;

    Ok(P::Compiler::supports_mode(
        &compiler_version,
        mode.optimize_setting,
        mode.pipeline,
    ))
}

async fn start_driver_task<'a, L, F>(
    args: &Arguments,
    tests: impl Stream<Item = Test<'a, L, F>>,
) -> anyhow::Result<impl Future<Output = ()>>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    info!("Starting driver task");

    let number_concurrent_tasks = args.number_of_concurrent_tasks();
    let cached_compiler = Arc::new(
        CachedCompiler::new(
            args.directory().join("compilation_cache"),
            args.invalidate_compilation_cache,
        )
        .await
        .context("Failed to initialize cached compiler")?,
    );

    Ok(tests.for_each_concurrent(
        // We want to limit the concurrent tasks here because:
        //
        // 1. We don't want to overwhelm the nodes with too many requests, leading to responses timing out.
        // 2. We don't want to open too many files at once, leading to the OS running out of file descriptors.
        //
        // By default, we allow maximum of 10 ongoing requests per node in order to limit (1), and assume that
        // this number will automatically be low enough to address (2). The user can override this.
        Some(number_concurrent_tasks),
        move |test| {
            let cached_compiler = cached_compiler.clone();

            async move {
                test.reporter
                    .report_leader_node_assigned_event(
                        test.leader_node.id(),
                        L::config_id(),
                        test.leader_node.connection_string(),
                    )
                    .expect("Can't fail");
                test.reporter
                    .report_follower_node_assigned_event(
                        test.follower_node.id(),
                        F::config_id(),
                        test.follower_node.connection_string(),
                    )
                    .expect("Can't fail");

                let reporter = test.reporter.clone();
                let result = handle_case_driver::<L, F>(test, args, cached_compiler).await;

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

#[allow(clippy::uninlined_format_args)]
#[allow(irrefutable_let_patterns)]
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
async fn handle_case_driver<L, F>(
    test: Test<'_, L, F>,
    config: &Arguments,
    cached_compiler: Arc<CachedCompiler>,
) -> anyhow::Result<usize>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_reporter = test
        .reporter
        .execution_specific_reporter(test.leader_node.id(), NodeDesignation::Leader);
    let follower_reporter = test
        .reporter
        .execution_specific_reporter(test.follower_node.id(), NodeDesignation::Follower);

    let (
        (
            CompilerOutput {
                contracts: leader_pre_link_contracts,
            },
            _,
        ),
        (
            CompilerOutput {
                contracts: follower_pre_link_contracts,
            },
            _,
        ),
    ) = try_join!(
        cached_compiler.compile_contracts::<L>(
            test.metadata,
            test.metadata_file_path,
            &test.mode,
            config,
            None,
            |compiler_version, compiler_path, is_cached, compiler_input, compiler_output| {
                leader_reporter
                    .report_pre_link_contracts_compilation_succeeded_event(
                        compiler_version,
                        compiler_path,
                        is_cached,
                        compiler_input,
                        compiler_output,
                    )
                    .expect("Can't fail")
            },
            |compiler_version, compiler_path, compiler_input, failure_reason| {
                leader_reporter
                    .report_pre_link_contracts_compilation_failed_event(
                        compiler_version,
                        compiler_path,
                        compiler_input,
                        failure_reason,
                    )
                    .expect("Can't fail")
            }
        ),
        cached_compiler.compile_contracts::<F>(
            test.metadata,
            test.metadata_file_path,
            &test.mode,
            config,
            None,
            |compiler_version, compiler_path, is_cached, compiler_input, compiler_output| {
                follower_reporter
                    .report_pre_link_contracts_compilation_succeeded_event(
                        compiler_version,
                        compiler_path,
                        is_cached,
                        compiler_input,
                        compiler_output,
                    )
                    .expect("Can't fail")
            },
            |compiler_version, compiler_path, compiler_input, failure_reason| {
                follower_reporter
                    .report_pre_link_contracts_compilation_failed_event(
                        compiler_version,
                        compiler_path,
                        compiler_input,
                        failure_reason,
                    )
                    .expect("Can't fail")
            }
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
        (
            CompilerOutput {
                contracts: leader_post_link_contracts,
            },
            leader_compiler_version,
        ),
        (
            CompilerOutput {
                contracts: follower_post_link_contracts,
            },
            follower_compiler_version,
        ),
    ) = try_join!(
        cached_compiler.compile_contracts::<L>(
            test.metadata,
            test.metadata_file_path,
            &test.mode,
            config,
            leader_deployed_libraries.as_ref(),
            |compiler_version, compiler_path, is_cached, compiler_input, compiler_output| {
                leader_reporter
                    .report_post_link_contracts_compilation_succeeded_event(
                        compiler_version,
                        compiler_path,
                        is_cached,
                        compiler_input,
                        compiler_output,
                    )
                    .expect("Can't fail")
            },
            |compiler_version, compiler_path, compiler_input, failure_reason| {
                leader_reporter
                    .report_post_link_contracts_compilation_failed_event(
                        compiler_version,
                        compiler_path,
                        compiler_input,
                        failure_reason,
                    )
                    .expect("Can't fail")
            }
        ),
        cached_compiler.compile_contracts::<F>(
            test.metadata,
            test.metadata_file_path,
            &test.mode,
            config,
            follower_deployed_libraries.as_ref(),
            |compiler_version, compiler_path, is_cached, compiler_input, compiler_output| {
                follower_reporter
                    .report_post_link_contracts_compilation_succeeded_event(
                        compiler_version,
                        compiler_path,
                        is_cached,
                        compiler_input,
                        compiler_output,
                    )
                    .expect("Can't fail")
            },
            |compiler_version, compiler_path, compiler_input, failure_reason| {
                follower_reporter
                    .report_post_link_contracts_compilation_failed_event(
                        compiler_version,
                        compiler_path,
                        compiler_input,
                        failure_reason,
                    )
                    .expect("Can't fail")
            }
        )
    )
    .context("Failed to compile post-link contracts for leader/follower in parallel")?;

    let leader_state = CaseState::<L>::new(
        leader_compiler_version,
        leader_post_link_contracts,
        leader_deployed_libraries.unwrap_or_default(),
        leader_reporter,
    );
    let follower_state = CaseState::<F>::new(
        follower_compiler_version,
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
    args: &Arguments,
    tests: &[MetadataFile],
    reporter: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    match (&args.leader, &args.follower) {
        (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
            run_driver::<Geth, Kitchensink>(args, tests, reporter, report_aggregator_task).await?
        }
        (TestingPlatform::Geth, TestingPlatform::Geth) => {
            run_driver::<Geth, Geth>(args, tests, reporter, report_aggregator_task).await?
        }
        _ => unimplemented!(),
    }

    Ok(())
}

async fn compile_corpus(
    config: &Arguments,
    tests: &[MetadataFile],
    platform: &TestingPlatform,
    _: Reporter,
    report_aggregator_task: impl Future<Output = anyhow::Result<()>>,
) {
    let tests = tests.iter().flat_map(|metadata| {
        metadata
            .solc_modes()
            .into_iter()
            .map(move |solc_mode| (metadata, solc_mode))
    });

    let file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let cached_compiler = CachedCompiler::new(file.path(), false)
        .await
        .map(Arc::new)
        .expect("Failed to create the cached compiler");

    let compilation_task =
        futures::stream::iter(tests).for_each_concurrent(None, |(metadata, mode)| {
            let cached_compiler = cached_compiler.clone();

            async move {
                match platform {
                    TestingPlatform::Geth => {
                        let _ = cached_compiler
                            .compile_contracts::<Geth>(
                                metadata,
                                metadata.metadata_file_path.as_path(),
                                &mode,
                                config,
                                None,
                                |_, _, _, _, _| {},
                                |_, _, _, _| {},
                            )
                            .await;
                    }
                    TestingPlatform::Kitchensink => {
                        let _ = cached_compiler
                            .compile_contracts::<Kitchensink>(
                                metadata,
                                metadata.metadata_file_path.as_path(),
                                &mode,
                                config,
                                None,
                                |_, _, _, _, _| {},
                                |_, _, _, _| {},
                            )
                            .await;
                    }
                }
            }
        });
    let _ = join!(compilation_task, report_aggregator_task);
}
