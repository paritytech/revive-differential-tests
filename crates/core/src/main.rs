use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, LazyLock, Mutex, RwLock},
};

use alloy::{
    json_abi::JsonAbi,
    network::{Ethereum, TransactionBuilder},
    primitives::Address,
    rpc::types::TransactionRequest,
};
use anyhow::Context;
use clap::Parser;
use rayon::{ThreadPoolBuilder, prelude::*};
use revive_dt_common::iterators::FilesWithExtensionIterator;
use revive_dt_node_interaction::EthereumNode;
use semver::Version;
use temp_dir::TempDir;
use tracing::Level;
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
    input::Input,
    metadata::{ContractInstance, ContractPathAndIdent, Metadata, MetadataFile},
    mode::SolcMode,
};
use revive_dt_node::pool::NodePool;
use revive_dt_report::reporter::{Report, Span};

static TEMP_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

type CompilationCache<'a> = Arc<
    RwLock<
        HashMap<
            (&'a Path, SolcMode, TestingPlatform),
            Arc<Mutex<Option<Arc<(Version, CompilerOutput)>>>>,
        >,
    >,
>;

fn main() -> anyhow::Result<()> {
    let args = init_cli()?;

    for (corpus, tests) in collect_corpora(&args)? {
        let span = Span::new(corpus, args.clone())?;

        match &args.compile_only {
            Some(platform) => compile_corpus(&args, &tests, platform, span),
            None => execute_corpus(&args, &tests, span)?,
        }

        Report::save()?;
    }

    Ok(())
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

    ThreadPoolBuilder::new()
        .num_threads(args.number_of_threads)
        .build_global()?;

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

fn run_driver<L, F>(args: &Arguments, tests: &[MetadataFile], span: Span) -> anyhow::Result<()>
where
    L: Platform,
    F: Platform,
    L::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
    F::Blockchain: revive_dt_node::Node + Send + Sync + 'static,
{
    let leader_nodes = NodePool::<L::Blockchain>::new(args)?;
    let follower_nodes = NodePool::<F::Blockchain>::new(args)?;

    let test_cases = tests
        .iter()
        .flat_map(
            |MetadataFile {
                 path,
                 content: metadata,
             }| {
                metadata
                    .cases
                    .iter()
                    .enumerate()
                    .flat_map(move |(case_idx, case)| {
                        metadata
                            .solc_modes()
                            .into_iter()
                            .map(move |solc_mode| (path, metadata, case_idx, case, solc_mode))
                    })
            },
        )
        .collect::<Vec<_>>();

    let compilation_cache = Arc::new(RwLock::new(HashMap::new()));
    test_cases.into_par_iter().for_each(
        |(metadata_file_path, metadata, case_idx, case, solc_mode)| {
            let tracing_span = tracing::span!(
                Level::INFO,
                "Running driver",
                metadata_file_path = %metadata_file_path.display(),
                case_idx = case_idx,
                solc_mode = ?solc_mode,
            );
            let _guard = tracing_span.enter();

            let result = handle_case_driver::<L, F>(
                metadata_file_path.as_path(),
                metadata,
                case_idx.into(),
                case,
                solc_mode,
                args,
                compilation_cache.clone(),
                leader_nodes.round_robbin(),
                follower_nodes.round_robbin(),
                span,
            );
            match result {
                Ok(inputs_executed) => tracing::info!(inputs_executed, "Execution succeeded"),
                Err(error) => tracing::info!(%error, "Execution failed"),
            }
            tracing::info!("Execution completed");
        },
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_case_driver<'a, L, F>(
    metadata_file_path: &'a Path,
    metadata: &'a Metadata,
    case_idx: CaseIdx,
    case: &Case,
    mode: SolcMode,
    config: &Arguments,
    compilation_cache: CompilationCache<'a>,
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
    )?;
    let follower_pre_link_contracts = get_or_build_contracts::<F>(
        metadata,
        metadata_file_path,
        mode.clone(),
        config,
        compilation_cache.clone(),
        &HashMap::new(),
    )?;

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
            .inputs
            .iter()
            .map(|input| input.caller)
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

        let leader_receipt = match leader_node.execute_transaction(leader_tx) {
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
        let follower_receipt = match follower_node.execute_transaction(follower_tx) {
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
            let leader_key = (metadata_file_path, mode.clone(), L::config_id());
            let follower_key = (metadata_file_path, mode.clone(), L::config_id());
            {
                let mut cache = compilation_cache.write().expect("Poisoned");
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
            )?;
            let follower_post_link_contracts = get_or_build_contracts::<F>(
                metadata,
                metadata_file_path,
                mode.clone(),
                config,
                compilation_cache,
                &follower_deployed_libraries,
            )?;

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
    driver.execute()
}

fn get_or_build_contracts<'a, P: Platform>(
    metadata: &'a Metadata,
    metadata_file_path: &'a Path,
    mode: SolcMode,
    config: &Arguments,
    compilation_cache: CompilationCache<'a>,
    deployed_libraries: &HashMap<ContractInstance, (Address, JsonAbi)>,
) -> anyhow::Result<Arc<(Version, CompilerOutput)>> {
    let key = (metadata_file_path, mode.clone(), P::config_id());
    if let Some(compilation_artifact) = compilation_cache
        .read()
        .expect("Poisoned")
        .get(&key)
        .cloned()
    {
        let mut compilation_artifact = compilation_artifact.lock().expect("Poisoned");
        match *compilation_artifact {
            Some(ref compiled_contracts) => {
                tracing::debug!(?key, "Compiled contracts cache hit");
                return Ok(compiled_contracts.clone());
            }
            None => {
                tracing::debug!(?key, "Compiled contracts cache miss");
                let compiled_contracts = Arc::new(compile_contracts::<P>(
                    metadata,
                    &mode,
                    config,
                    deployed_libraries,
                )?);
                *compilation_artifact = Some(compiled_contracts.clone());
                return Ok(compiled_contracts.clone());
            }
        }
    };

    tracing::debug!(?key, "Compiled contracts cache miss");
    let mutex = {
        let mut compilation_cache = compilation_cache.write().expect("Poisoned");
        let mutex = Arc::new(Mutex::new(None));
        compilation_cache.insert(key, mutex.clone());
        mutex
    };
    let mut compilation_artifact = mutex.lock().expect("Poisoned");
    let compiled_contracts = Arc::new(compile_contracts::<P>(
        metadata,
        &mode,
        config,
        deployed_libraries,
    )?);
    *compilation_artifact = Some(compiled_contracts.clone());
    Ok(compiled_contracts.clone())
}

fn compile_contracts<P: Platform>(
    metadata: &Metadata,
    mode: &SolcMode,
    config: &Arguments,
    deployed_libraries: &HashMap<ContractInstance, (Address, JsonAbi)>,
) -> anyhow::Result<(Version, CompilerOutput)> {
    let compiler_version_or_requirement = mode.compiler_version_to_use(config.solc.clone());
    let compiler_path =
        P::Compiler::get_compiler_executable(config, compiler_version_or_requirement)?;
    let compiler_version = P::Compiler::new(compiler_path.clone()).version()?;

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

        // Note the following: we need to tell solc which files require the libraries to be
        // linked into them. We do not have access to this information and therefore we choose
        // an easier, yet more compute intensive route, of telling solc that all of the files
        // need to link the library and it will only perform the linking for the files that do
        // actually need the library.
        compiler = FilesWithExtensionIterator::new(metadata.directory()?)
            .with_allowed_extension("sol")
            .fold(compiler, |compiler, path| {
                compiler.with_library(&path, library_ident.as_str(), *library_address)
            });
    }

    let compiler_output = compiler.try_build(compiler_path)?;

    Ok((compiler_version, compiler_output))
}

fn execute_corpus(args: &Arguments, tests: &[MetadataFile], span: Span) -> anyhow::Result<()> {
    match (&args.leader, &args.follower) {
        (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
            run_driver::<Geth, Kitchensink>(args, tests, span)?
        }
        (TestingPlatform::Geth, TestingPlatform::Geth) => {
            run_driver::<Geth, Geth>(args, tests, span)?
        }
        _ => unimplemented!(),
    }

    Ok(())
}

fn compile_corpus(config: &Arguments, tests: &[MetadataFile], platform: &TestingPlatform, _: Span) {
    tests.par_iter().for_each(|metadata| {
        for mode in &metadata.solc_modes() {
            match platform {
                TestingPlatform::Geth => {
                    let _ = compile_contracts::<Geth>(
                        &metadata.content,
                        mode,
                        config,
                        &Default::default(),
                    );
                }
                TestingPlatform::Kitchensink => {
                    let _ = compile_contracts::<Geth>(
                        &metadata.content,
                        mode,
                        config,
                        &Default::default(),
                    );
                }
            };
        }
    });
}
