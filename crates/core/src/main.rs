use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Instant,
};

use alloy::{
    json_abi::JsonAbi,
    network::{Ethereum, TransactionBuilder},
    primitives::Address,
    rpc::types::TransactionRequest,
};
use anyhow::Context;
use clap::Parser;
use futures::StreamExt;
use revive_dt_common::iterators::FilesWithExtensionIterator;
use revive_dt_node_interaction::EthereumNode;
use semver::Version;
use temp_dir::TempDir;
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{Instrument, Level};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use revive_dt_compiler::SolidityCompiler;
use revive_dt_compiler::{Compiler, CompilerOutput};
use revive_dt_config::*;
use revive_dt_core::{
    Geth, Kitchensink, Platform,
    driver::{CaseDriver, CaseState},
};
use revive_dt_format::{
    case::{Case, CaseIdx},
    corpus::Corpus,
    input::{Input, Step},
    metadata::{ContractInstance, ContractPathAndIdent, Metadata, MetadataFile},
    mode::SolcMode,
};
use revive_dt_node::pool::NodePool;
use revive_dt_report::reporter::{Report, Span};

static TEMP_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

type CompilationCache = Arc<
    RwLock<
        HashMap<
            (PathBuf, SolcMode, TestingPlatform),
            Arc<Mutex<Option<Arc<(Version, CompilerOutput)>>>>,
        >,
    >,
>;

/// this represents a single "test"; a mode, path and collection of cases.
#[derive(Clone)]
struct Test {
    metadata: Metadata,
    path: PathBuf,
    mode: SolcMode,
    cases: Vec<Case>,
}

/// This represents the results that we gather from running test cases.
type CaseResults = Vec<Result<usize, anyhow::Error>>;

fn main() -> anyhow::Result<()> {
    let args = init_cli()?;

    let body = async {
        for (corpus, tests) in collect_corpora(&args)? {
            let span = Span::new(corpus, args.clone())?;
            match &args.compile_only {
                Some(platform) => compile_corpus(&args, &tests, platform, span).await,
                None => execute_corpus(&args, &tests, span).await?,
            }
            Report::save()?;
        }
        Ok(())
    };

    let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();

    if let Some(num_threads) = args.number_of_threads {
        // Allow it to default to number of CPU cores if not specified.
        runtime_builder.worker_threads(num_threads);
    }

    runtime_builder
        .enable_all()
        .build()
        .expect("Failed building the Runtime")
        .block_on(body)
}

fn init_cli() -> anyhow::Result<Arguments> {
    let subscriber = FmtSubscriber::builder()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .pretty()
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

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
    tracing::info!("workdir: {}", args.directory().display());

    Ok(args)
}

fn collect_corpora(args: &Arguments) -> anyhow::Result<HashMap<Corpus, Vec<MetadataFile>>> {
    let mut corpora = HashMap::new();

    for path in &args.corpus {
        let corpus = Corpus::try_from_path(path)?;
        tracing::info!("found corpus: {}", path.display());
        let tests = corpus.enumerate_tests();
        tracing::info!("corpus '{}' contains {} tests", &corpus.name, tests.len());
        corpora.insert(corpus, tests);
    }

    Ok(corpora)
}

async fn run_driver<L, F>(
    args: &Arguments,
    metadata_files: &[MetadataFile],
    span: Span,
) -> anyhow::Result<()>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let (report_tx, report_rx) = mpsc::unbounded_channel::<(Test, CaseResults)>();

    let tests = prepare_tests::<L, F>(metadata_files);
    let driver_task = spawn_driver_task::<L, F>(args, tests, span, report_tx)?;
    let status_reporter_task = spawn_reporter_task(report_rx);

    tokio::join!(status_reporter_task, driver_task);

    Ok(())
}

fn prepare_tests<L, F>(metadata_files: &[MetadataFile]) -> impl Iterator<Item = Test>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    metadata_files
        .iter()
        .filter(|file| match file.content.required_evm_version {
            Some(evm_version_requirement) => {
                let is_allowed = evm_version_requirement
                    .matches(&<L::Blockchain as revive_dt_node::Node>::evm_version())
                    && evm_version_requirement
                        .matches(&<F::Blockchain as revive_dt_node::Node>::evm_version());

                if !is_allowed {
                    tracing::warn!(
                        metadata_file_path = %file.path.display(),
                        leader_evm_version = %<L::Blockchain as revive_dt_node::Node>::evm_version(),
                        follower_evm_version = %<F::Blockchain as revive_dt_node::Node>::evm_version(),
                        version_requirement = %evm_version_requirement,
                        "Skipped test since the EVM version requirement was not fulfilled."
                    );
                }

                is_allowed
            }
            None => true,
        })
        .flat_map(|MetadataFile { path, content: metadata }| {
            let cases = metadata
                .cases
                .iter()
                .enumerate()
                .filter_map(|(case_idx, case)| {
                    if case.ignore.unwrap_or_default() {
                        tracing::warn!(
                            metadata_file_path = %path.display(),
                            case_idx,
                            case_name = ?case.name,
                            "Ignoring case"
                        );
                        None
                    } else {
                        Some(case.clone())
                    }
                })
                .collect::<Vec<_>>();

            metadata.solc_modes().into_iter().map(move |solc_mode| Test {
                metadata: metadata.clone(),
                path: path.to_path_buf(),
                mode: solc_mode,
                cases: cases.clone(),
            })
        })
}

fn spawn_driver_task<L, F>(
    args: &Arguments,
    tests: impl Iterator<Item = Test>,
    span: Span,
    report_tx: mpsc::UnboundedSender<(Test, CaseResults)>,
) -> anyhow::Result<impl Future<Output = ()>>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_nodes = Arc::new(NodePool::<L::Blockchain>::new(args)?);
    let follower_nodes = Arc::new(NodePool::<F::Blockchain>::new(args)?);
    let compilation_cache = Arc::new(RwLock::new(HashMap::new()));
    let number_concurrent_tasks = args
        .number_concurrent_tasks
        .unwrap_or(args.number_of_nodes * 10);

    Ok(futures::stream::iter(tests).for_each_concurrent(
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
            let compilation_cache = compilation_cache.clone();
            let report_tx = report_tx.clone();

            async move {
                let leader_node = leader_nodes.round_robbin();
                let follower_node = follower_nodes.round_robbin();

                let mut case_results = vec![];
                for (case_idx, case) in test.cases.iter().enumerate() {
                    let tracing_span = tracing::span!(
                        Level::INFO,
                        "Running driver",
                        metadata_file_path = %test.path.display(),
                        case_idx = case_idx,
                        solc_mode = ?test.mode,
                    );

                    let result = handle_case_driver::<L, F>(
                        &test.path,
                        &test.metadata,
                        case_idx.into(),
                        case,
                        test.mode.clone(),
                        args,
                        compilation_cache.clone(),
                        leader_node,
                        follower_node,
                        span,
                    )
                    .instrument(tracing_span)
                    .await;

                    case_results.push(result);
                }

                report_tx
                    .send((test, case_results))
                    .expect("Failed to send report");
            }
        },
    ))
}

async fn spawn_reporter_task(mut report_rx: mpsc::UnboundedReceiver<(Test, CaseResults)>) {
    let start = Instant::now();

    const GREEN: &str = "\x1B[32m";
    const RED: &str = "\x1B[31m";
    const RESET: &str = "\x1B[0m";

    let mut number_of_successes = 0;
    let mut number_of_failures = 0;

    // Wait for reports to come from our test runner. When the channel closes, this ends.
    while let Some((test, case_results)) = report_rx.recv().await {
        let tracing_span = tracing::span!(
            Level::INFO,
            "Test completed",
            metadata_file_path = %test.path.display(),
            solc_mode = ?test.mode,
        );
        let _ = tracing_span.enter();

        let contains_failures = case_results.iter().any(|res| res.is_err());

        if !contains_failures {
            eprintln!(
                "{}Succeeded:{} {} - {:?}",
                GREEN,
                RESET,
                test.path.display(),
                test.mode
            )
        } else {
            eprintln!(
                "{}Failed:{} {} - {:?}",
                RED,
                RESET,
                test.path.display(),
                test.mode
            )
        };

        for (case_idx, (case, result)) in test.cases.iter().zip(case_results).enumerate() {
            let case_name = case.name.as_deref().unwrap_or("Unnamed case");

            match result {
                Ok(_inputs_executed) => {
                    number_of_successes += 1;
                    eprintln!("  {GREEN}Case Succeeded:{RESET} {case_name} - Case Idx: {case_idx}",);
                }
                Err(err) => {
                    number_of_failures += 1;
                    eprintln!("  {RED}Case Failed:{RESET} {case_name} - Case Idx: {case_idx}",);
                    tracing::info!(
                        case_idx,
                        case_name,
                        %err,
                        "Case failed with error"
                    )
                }
            }
        }
        eprintln!();
    }

    let elapsed = start.elapsed();
    eprintln!(
        "{GREEN}{}{RESET} cases succeeded, {RED}{}{RESET} cases failed in {} seconds",
        number_of_successes,
        number_of_failures,
        elapsed.as_secs()
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_case_driver<L, F>(
    metadata_file_path: &Path,
    metadata: &Metadata,
    case_idx: CaseIdx,
    case: &Case,
    mode: SolcMode,
    config: &Arguments,
    compilation_cache: CompilationCache,
    leader_node: &L::Blockchain,
    follower_node: &F::Blockchain,
    _: Span,
) -> anyhow::Result<usize>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_pre_link_contracts = get_or_build_contracts::<L>(
        metadata,
        metadata_file_path,
        mode.clone(),
        config,
        compilation_cache.clone(),
        &HashMap::new(),
    )
    .await?;
    let follower_pre_link_contracts = get_or_build_contracts::<F>(
        metadata,
        metadata_file_path,
        mode.clone(),
        config,
        compilation_cache.clone(),
        &HashMap::new(),
    )
    .await?;

    let mut leader_deployed_libraries = HashMap::new();
    let mut follower_deployed_libraries = HashMap::new();
    let mut contract_sources = metadata.contract_sources()?;
    for library_instance in metadata
        .libraries
        .iter()
        .flatten()
        .flat_map(|(_, map)| map.values())
    {
        let ContractPathAndIdent {
            contract_source_path: library_source_path,
            contract_ident: library_ident,
        } = contract_sources
            .remove(library_instance)
            .context("Failed to find the contract source")?;

        let (leader_code, leader_abi) = leader_pre_link_contracts
            .1
            .contracts
            .get(&library_source_path)
            .and_then(|contracts| contracts.get(library_ident.as_str()))
            .context("Declared library was not compiled")?;
        let (follower_code, follower_abi) = follower_pre_link_contracts
            .1
            .contracts
            .get(&library_source_path)
            .and_then(|contracts| contracts.get(library_ident.as_str()))
            .context("Declared library was not compiled")?;

        let leader_code = match alloy::hex::decode(leader_code) {
            Ok(code) => code,
            Err(error) => {
                tracing::error!(
                    ?error,
                    contract_source_path = library_source_path.display().to_string(),
                    contract_ident = library_ident.as_ref(),
                    "Failed to hex-decode byte code - This could possibly mean that the bytecode requires linking"
                );
                anyhow::bail!("Failed to hex-decode the byte code {}", error)
            }
        };
        let follower_code = match alloy::hex::decode(follower_code) {
            Ok(code) => code,
            Err(error) => {
                tracing::error!(
                    ?error,
                    contract_source_path = library_source_path.display().to_string(),
                    contract_ident = library_ident.as_ref(),
                    "Failed to hex-decode byte code - This could possibly mean that the bytecode requires linking"
                );
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

        let leader_receipt = match leader_node.execute_transaction(leader_tx).await {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::error!(
                    node = std::any::type_name::<L>(),
                    ?error,
                    "Contract deployment transaction failed."
                );
                return Err(error);
            }
        };
        let follower_receipt = match follower_node.execute_transaction(follower_tx).await {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::error!(
                    node = std::any::type_name::<F>(),
                    ?error,
                    "Contract deployment transaction failed."
                );
                return Err(error);
            }
        };

        tracing::info!(
            ?library_instance,
            library_address = ?leader_receipt.contract_address,
            "Deployed library to leader"
        );
        tracing::info!(
            ?library_instance,
            library_address = ?follower_receipt.contract_address,
            "Deployed library to follower"
        );

        let Some(leader_library_address) = leader_receipt.contract_address else {
            tracing::error!("Contract deployment transaction didn't return an address");
            anyhow::bail!("Contract deployment didn't return an address");
        };
        let Some(follower_library_address) = follower_receipt.contract_address else {
            tracing::error!("Contract deployment transaction didn't return an address");
            anyhow::bail!("Contract deployment didn't return an address");
        };

        leader_deployed_libraries.insert(
            library_instance.clone(),
            (leader_library_address, leader_abi.clone()),
        );
        follower_deployed_libraries.insert(
            library_instance.clone(),
            (follower_library_address, follower_abi.clone()),
        );
    }

    let metadata_file_contains_libraries = metadata
        .libraries
        .iter()
        .flat_map(|map| map.iter())
        .flat_map(|(_, value)| value.iter())
        .next()
        .is_some();
    let compiled_contracts_require_linking = leader_pre_link_contracts
        .1
        .contracts
        .values()
        .chain(follower_pre_link_contracts.1.contracts.values())
        .flat_map(|value| value.values())
        .any(|(code, _)| !code.chars().all(|char| char.is_ascii_hexdigit()));
    let (leader_compiled_contracts, follower_compiled_contracts) =
        if metadata_file_contains_libraries && compiled_contracts_require_linking {
            let leader_key = (
                metadata_file_path.to_path_buf(),
                mode.clone(),
                L::config_id(),
            );
            let follower_key = (
                metadata_file_path.to_path_buf(),
                mode.clone(),
                L::config_id(),
            );
            {
                let mut cache = compilation_cache.write().await;
                cache.remove(&leader_key);
                cache.remove(&follower_key);
            }

            let leader_post_link_contracts = get_or_build_contracts::<L>(
                metadata,
                metadata_file_path,
                mode.clone(),
                config,
                compilation_cache.clone(),
                &leader_deployed_libraries,
            )
            .await?;
            let follower_post_link_contracts = get_or_build_contracts::<F>(
                metadata,
                metadata_file_path,
                mode.clone(),
                config,
                compilation_cache,
                &follower_deployed_libraries,
            )
            .await?;

            (leader_post_link_contracts, follower_post_link_contracts)
        } else {
            (leader_pre_link_contracts, follower_pre_link_contracts)
        };

    let leader_state = CaseState::<L>::new(
        leader_compiled_contracts.0.clone(),
        leader_compiled_contracts.1.contracts.clone(),
        leader_deployed_libraries,
    );
    let follower_state = CaseState::<F>::new(
        follower_compiled_contracts.0.clone(),
        follower_compiled_contracts.1.contracts.clone(),
        follower_deployed_libraries,
    );

    let mut driver = CaseDriver::<L, F>::new(
        metadata,
        case,
        case_idx,
        leader_node,
        follower_node,
        leader_state,
        follower_state,
    );
    driver.execute().await
}

async fn get_or_build_contracts<P: Platform>(
    metadata: &Metadata,
    metadata_file_path: &Path,
    mode: SolcMode,
    config: &Arguments,
    compilation_cache: CompilationCache,
    deployed_libraries: &HashMap<ContractInstance, (Address, JsonAbi)>,
) -> anyhow::Result<Arc<(Version, CompilerOutput)>> {
    let key = (
        metadata_file_path.to_path_buf(),
        mode.clone(),
        P::config_id(),
    );
    if let Some(compilation_artifact) = compilation_cache.read().await.get(&key).cloned() {
        let mut compilation_artifact = compilation_artifact.lock().await;
        match *compilation_artifact {
            Some(ref compiled_contracts) => {
                tracing::debug!(?key, "Compiled contracts cache hit");
                return Ok(compiled_contracts.clone());
            }
            None => {
                tracing::debug!(?key, "Compiled contracts cache miss");
                let compiled_contracts = Arc::new(
                    compile_contracts::<P>(
                        metadata,
                        metadata_file_path,
                        &mode,
                        config,
                        deployed_libraries,
                    )
                    .await?,
                );
                *compilation_artifact = Some(compiled_contracts.clone());
                return Ok(compiled_contracts.clone());
            }
        }
    };

    tracing::debug!(?key, "Compiled contracts cache miss");
    let mutex = {
        let mut compilation_cache = compilation_cache.write().await;
        let mutex = Arc::new(Mutex::new(None));
        compilation_cache.insert(key, mutex.clone());
        mutex
    };
    let mut compilation_artifact = mutex.lock().await;
    let compiled_contracts = Arc::new(
        compile_contracts::<P>(
            metadata,
            metadata_file_path,
            &mode,
            config,
            deployed_libraries,
        )
        .await?,
    );
    *compilation_artifact = Some(compiled_contracts.clone());
    Ok(compiled_contracts.clone())
}

async fn compile_contracts<P: Platform>(
    metadata: &Metadata,
    metadata_file_path: &Path,
    mode: &SolcMode,
    config: &Arguments,
    deployed_libraries: &HashMap<ContractInstance, (Address, JsonAbi)>,
) -> anyhow::Result<(Version, CompilerOutput)> {
    let compiler_version_or_requirement = mode.compiler_version_to_use(config.solc.clone());
    let compiler_path =
        P::Compiler::get_compiler_executable(config, compiler_version_or_requirement).await?;
    let compiler_version = P::Compiler::new(compiler_path.clone()).version()?;

    tracing::info!(
        %compiler_version,
        metadata_file_path = %metadata_file_path.display(),
        mode = ?mode,
        "Compiling contracts"
    );

    let compiler = Compiler::<P::Compiler>::new()
        .with_allow_path(metadata.directory()?)
        .with_optimization(mode.solc_optimize());
    let mut compiler = metadata
        .files_to_compile()?
        .try_fold(compiler, |compiler, path| compiler.with_source(&path))?;
    for (library_instance, (library_address, _)) in deployed_libraries.iter() {
        let library_ident = &metadata
            .contracts
            .as_ref()
            .and_then(|contracts| contracts.get(library_instance))
            .expect("Impossible for library to not be found in contracts")
            .contract_ident;

        // Note the following: we need to tell solc which files require the libraries to be linked
        // into them. We do not have access to this information and therefore we choose an easier,
        // yet more compute intensive route, of telling solc that all of the files need to link the
        // library and it will only perform the linking for the files that do actually need the
        // library.
        compiler = FilesWithExtensionIterator::new(metadata.directory()?)
            .with_allowed_extension("sol")
            .fold(compiler, |compiler, path| {
                compiler.with_library(&path, library_ident.as_str(), *library_address)
            });
    }

    let compiler_output = compiler.try_build(compiler_path).await?;

    Ok((compiler_version, compiler_output))
}

async fn execute_corpus(
    args: &Arguments,
    tests: &[MetadataFile],
    span: Span,
) -> anyhow::Result<()> {
    match (&args.leader, &args.follower) {
        (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
            run_driver::<Geth, Kitchensink>(args, tests, span).await?
        }
        (TestingPlatform::Geth, TestingPlatform::Geth) => {
            run_driver::<Geth, Geth>(args, tests, span).await?
        }
        _ => unimplemented!(),
    }

    Ok(())
}

async fn compile_corpus(
    config: &Arguments,
    tests: &[MetadataFile],
    platform: &TestingPlatform,
    _: Span,
) {
    let tests = tests.iter().flat_map(|metadata| {
        metadata
            .solc_modes()
            .into_iter()
            .map(move |solc_mode| (metadata, solc_mode))
    });

    futures::stream::iter(tests)
        .for_each_concurrent(None, |(metadata, mode)| async move {
            match platform {
                TestingPlatform::Geth => {
                    let _ = compile_contracts::<Geth>(
                        &metadata.content,
                        &metadata.path,
                        &mode,
                        config,
                        &Default::default(),
                    )
                    .await;
                }
                TestingPlatform::Kitchensink => {
                    let _ = compile_contracts::<Geth>(
                        &metadata.content,
                        &metadata.path,
                        &mode,
                        config,
                        &Default::default(),
                    )
                    .await;
                }
            }
        })
        .await;
}
