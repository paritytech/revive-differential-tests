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
use temp_dir::TempDir;
use tokio::{sync::mpsc, try_join};
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
#[derive(Clone, Debug)]
struct Test<'a> {
    metadata: &'a MetadataFile,
    metadata_file_path: &'a Path,
    mode: Mode,
    case_idx: CaseIdx,
    case: &'a Case,
}

/// This represents the results that we gather from running test cases.
type CaseResult = Result<usize, anyhow::Error>;

fn main() -> anyhow::Result<()> {
    let (args, _guard) = init_cli()?;
    info!(
        leader = args.leader.to_string(),
        follower = args.follower.to_string(),
        working_directory = %args.directory().display(),
        number_of_nodes = args.number_of_nodes,
        invalidate_compilation_cache = args.invalidate_compilation_cache,
        "Differential testing tool has been initialized"
    );

    let body = async {
        for (_, tests) in collect_corpora(&args)? {
            match &args.compile_only {
                Some(platform) => compile_corpus(&args, &tests, platform).await,
                None => execute_corpus(&args, &tests).await?,
            }
        }
        Ok(())
    };

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.number_of_threads)
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

async fn run_driver<L, F>(args: &Arguments, metadata_files: &[MetadataFile]) -> anyhow::Result<()>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let (report_tx, report_rx) = mpsc::unbounded_channel::<(Test<'_>, CaseResult)>();

    let tests = prepare_tests::<L, F>(args, metadata_files);
    let driver_task = start_driver_task::<L, F>(args, tests, report_tx).await?;
    let status_reporter_task = start_reporter_task(report_rx);

    tokio::join!(status_reporter_task, driver_task);

    Ok(())
}

fn prepare_tests<'a, L, F>(
    args: &Arguments,
    metadata_files: &'a [MetadataFile],
) -> impl Stream<Item = Test<'a>>
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
        .fold(
            IndexMap::<_, BTreeMap<_, Vec<_>>>::new(),
            |mut map, (metadata_file, case_idx, case, mode)| {
                let test = Test {
                    metadata: metadata_file,
                    metadata_file_path: metadata_file.metadata_file_path.as_path(),
                    mode: mode.clone(),
                    case_idx: CaseIdx::new(case_idx),
                    case,
                };
                map.entry(mode)
                    .or_default()
                    .entry(test.case_idx)
                    .or_default()
                    .push(test);
                map
            },
        )
        .into_values()
        .flatten()
        .flat_map(|(_, value)| value.into_iter())
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
                )
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
            }

            is_allowed.then_some(test)
        })
}

async fn does_compiler_support_mode<P: Platform>(
    args: &Arguments,
    mode: &Mode,
) -> anyhow::Result<bool> {
    let compiler_version_or_requirement = mode.compiler_version_to_use(args.solc.clone());
    let compiler_path =
        P::Compiler::get_compiler_executable(args, compiler_version_or_requirement).await?;
    let compiler_version = P::Compiler::new(compiler_path.clone()).version().await?;

    Ok(P::Compiler::supports_mode(
        &compiler_version,
        mode.optimize_setting,
        mode.pipeline,
    ))
}

async fn start_driver_task<'a, L, F>(
    args: &Arguments,
    tests: impl Stream<Item = Test<'a>>,
    report_tx: mpsc::UnboundedSender<(Test<'a>, CaseResult)>,
) -> anyhow::Result<impl Future<Output = ()>>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_nodes = Arc::new(NodePool::<L::Blockchain>::new(args)?);
    let follower_nodes = Arc::new(NodePool::<F::Blockchain>::new(args)?);
    let number_concurrent_tasks = args.number_of_concurrent_tasks();
    let cached_compiler = Arc::new(
        CachedCompiler::new(
            args.directory().join("compilation_cache"),
            args.invalidate_compilation_cache,
        )
        .await?,
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
            let leader_nodes = leader_nodes.clone();
            let follower_nodes = follower_nodes.clone();
            let report_tx = report_tx.clone();
            let cached_compiler = cached_compiler.clone();

            async move {
                let leader_node = leader_nodes.round_robbin();
                let follower_node = follower_nodes.round_robbin();

                let result = handle_case_driver::<L, F>(
                    test.metadata_file_path,
                    test.metadata,
                    test.case_idx,
                    test.case,
                    test.mode.clone(),
                    args,
                    cached_compiler,
                    leader_node,
                    follower_node,
                )
                .await;

                report_tx
                    .send((test, result))
                    .expect("Failed to send report");
            }
        },
    ))
}

async fn start_reporter_task(mut report_rx: mpsc::UnboundedReceiver<(Test<'_>, CaseResult)>) {
    let start = Instant::now();

    const GREEN: &str = "\x1B[32m";
    const RED: &str = "\x1B[31m";
    const COLOUR_RESET: &str = "\x1B[0m";
    const BOLD: &str = "\x1B[1m";
    const BOLD_RESET: &str = "\x1B[22m";

    let mut number_of_successes = 0;
    let mut number_of_failures = 0;
    let mut failures = vec![];

    // Wait for reports to come from our test runner. When the channel closes, this ends.
    let mut buf = BufWriter::new(stderr());
    while let Some((test, case_result)) = report_rx.recv().await {
        let case_name = test.case.name.as_deref().unwrap_or("unnamed_case");
        let case_idx = test.case_idx;
        let test_path = test.metadata_file_path.display();
        let test_mode = test.mode.clone();

        match case_result {
            Ok(_inputs) => {
                number_of_successes += 1;
                let _ = writeln!(
                    buf,
                    "{GREEN}Case Succeeded:{COLOUR_RESET} {test_path} -> {case_name}:{case_idx} (mode: {test_mode})"
                );
            }
            Err(err) => {
                number_of_failures += 1;
                let _ = writeln!(
                    buf,
                    "{RED}Case Failed:{COLOUR_RESET} {test_path} -> {case_name}:{case_idx} (mode: {test_mode})"
                );
                failures.push((test, err));
            }
        }
    }

    let _ = writeln!(buf,);
    let elapsed = start.elapsed();

    // Now, log the failures with more complete errors at the bottom, like `cargo test` does, so
    // that we don't have to scroll through the entire output to find them.
    if !failures.is_empty() {
        let _ = writeln!(buf, "{BOLD}Failures:{BOLD_RESET}\n");

        for failure in failures {
            let (test, err) = failure;
            let case_name = test.case.name.as_deref().unwrap_or("unnamed_case");
            let case_idx = test.case_idx;
            let test_path = test.metadata_file_path.display();
            let test_mode = test.mode.clone();

            let _ = writeln!(
                buf,
                "---- {RED}Case Failed:{COLOUR_RESET} {test_path} -> {case_name}:{case_idx} (mode: {test_mode}) ----\n\n{err}\n"
            );
        }
    }

    // Summary at the end.
    let _ = writeln!(
        buf,
        "{} cases: {GREEN}{number_of_successes}{COLOUR_RESET} cases succeeded, {RED}{number_of_failures}{COLOUR_RESET} cases failed in {} seconds",
        number_of_successes + number_of_failures,
        elapsed.as_secs()
    );
}

#[allow(clippy::too_many_arguments)]
#[instrument(
    level = "info",
    name = "Handling Case"
    skip_all,
    fields(
        metadata_file_path = %metadata.relative_path().display(),
        mode = %mode,
        %case_idx,
        case_name = case.name.as_deref().unwrap_or("Unnamed Case"),
        leader_node = leader_node.id(),
        follower_node = follower_node.id(),
    )
)]
async fn handle_case_driver<L, F>(
    metadata_file_path: &Path,
    metadata: &MetadataFile,
    case_idx: CaseIdx,
    case: &Case,
    mode: Mode,
    config: &Arguments,
    cached_compiler: Arc<CachedCompiler>,
    leader_node: &L::Blockchain,
    follower_node: &F::Blockchain,
) -> anyhow::Result<usize>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
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
        cached_compiler.compile_contracts::<L>(metadata, metadata_file_path, &mode, config, None),
        cached_compiler.compile_contracts::<F>(metadata, metadata_file_path, &mode, config, None)
    )?;

    let mut leader_deployed_libraries = None::<HashMap<_, _>>;
    let mut follower_deployed_libraries = None::<HashMap<_, _>>;
    let mut contract_sources = metadata.contract_sources()?;
    for library_instance in metadata
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
        let deployer_address = case
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
            leader_node.execute_transaction(leader_tx),
            follower_node.execute_transaction(follower_tx)
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
            metadata,
            metadata_file_path,
            &mode,
            config,
            leader_deployed_libraries.as_ref()
        ),
        cached_compiler.compile_contracts::<F>(
            metadata,
            metadata_file_path,
            &mode,
            config,
            follower_deployed_libraries.as_ref()
        )
    )?;

    let leader_state = CaseState::<L>::new(
        leader_compiler_version,
        leader_post_link_contracts,
        leader_deployed_libraries.unwrap_or_default(),
    );
    let follower_state = CaseState::<F>::new(
        follower_compiler_version,
        follower_post_link_contracts,
        follower_deployed_libraries.unwrap_or_default(),
    );

    let mut driver = CaseDriver::<L, F>::new(
        metadata,
        case,
        leader_node,
        follower_node,
        leader_state,
        follower_state,
    );
    driver
        .execute()
        .await
        .inspect(|steps_executed| info!(steps_executed, "Case succeeded"))
}

async fn execute_corpus(args: &Arguments, tests: &[MetadataFile]) -> anyhow::Result<()> {
    match (&args.leader, &args.follower) {
        (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
            run_driver::<Geth, Kitchensink>(args, tests).await?
        }
        (TestingPlatform::Geth, TestingPlatform::Geth) => {
            run_driver::<Geth, Geth>(args, tests).await?
        }
        _ => unimplemented!(),
    }

    Ok(())
}

async fn compile_corpus(config: &Arguments, tests: &[MetadataFile], platform: &TestingPlatform) {
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

    futures::stream::iter(tests)
        .for_each_concurrent(None, |(metadata, mode)| {
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
                            )
                            .await;
                    }
                }
            }
        })
        .await;
}
