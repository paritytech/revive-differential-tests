use crate::internal_prelude::*;

mod execution;
pub(crate) use execution::{
    Profiling, ProfilingReport, TransactionProfilingReport, WorkloadProfilingReport,
};

pub(crate) async fn handle_profiling(context: Profile) -> Result<()> {
    let allowed_modes = ModeAllowList::from_parsed_modes(context.corpus.allowed_modes.iter());
    let corpus = context
        .corpus
        .test_specifiers
        .iter()
        .cloned()
        .try_fold(Corpus::new(), Corpus::with_test_specifier)?;
    let built = build_profiling_runtime(&context.profiling.runtime_branch).await?;
    let mut report = ProfilingReport {
        runtime_branch: context.profiling.runtime_branch.clone(),
        runtime_commit: built.commit.to_string(),
        workloads: Vec::new(),
    };
    let runtime = ProfilingRuntime::new(instrument_wasm(built.wasm)?);
    let signers = context.wallet.signers()?;
    let mut compilers = HashMap::new();

    for (metadata, case_index, case, mode) in corpus.cases_iterator() {
        if !allowed_modes.allows(&mode)
            || metadata.ignore == Some(true)
            || case.ignore == Some(true)
            || metadata
                .targets
                .as_ref()
                .is_some_and(|targets| !targets.contains(&VmIdentifier::Evm))
            || case
                .targets
                .as_ref()
                .is_some_and(|targets| !targets.contains(&VmIdentifier::Evm))
        {
            continue;
        }
        let compiler = match compilers.entry(mode.solc_version.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(
                new_solc_compiler(
                    context.solc.clone(),
                    context.working_directory.clone(),
                    mode.solc_version
                        .clone()
                        .map(VersionOrRequirement::Requirement),
                )
                .await?,
            ),
        };
        if !compiler.supports_mode(&mode) {
            continue;
        }
        let span = info_span!("Profiling workload", path = %metadata.metadata_file_path.display(),
            case = %case_index, mode = %mode);
        let _entered = span.enter();
        let mut profiling = Profiling::new(&runtime, &signers, &context.wallet)?;
        let sources = metadata.contract_sources(CompilerIdentifier::Solc)?;
        let mut input = metadata
            .files_to_compile()?
            .try_fold(
                Compiler::new()
                    .with_metadata_file_path(&metadata.metadata_file_path)
                    .with_base_path(metadata.directory()?)
                    .with_allow_path(metadata.directory()?)
                    .with_pipeline(mode.pipeline)
                    .with_optimization(mode.optimize_setting),
                |compiler, source| compiler.with_source(source),
            )?
            .input()
            .clone();
        input.revert_string_handling = metadata
            .compiler_directives
            .as_ref()
            .and_then(|directives| directives.revert_string_handling)
            .map(|handling| match handling {
                WorkloadRevertString::Default => CompilerRevertString::Default,
                WorkloadRevertString::Debug => CompilerRevertString::Debug,
                WorkloadRevertString::Strip => CompilerRevertString::Strip,
                WorkloadRevertString::VerboseDebug => CompilerRevertString::VerboseDebug,
            });
        let mut compiled = compile(compiler.as_ref(), &input, &context).await?;
        for (path, libraries) in metadata.libraries.iter().flatten() {
            for (name, instance) in libraries {
                let source = sources
                    .get(instance)
                    .context("Library instance has no source")?;
                let (bytecode, abi) = compiled
                    .contracts
                    .get(&source.contract_source_path)
                    .and_then(|contracts| contracts.get(source.contract_ident.as_str()))
                    .context("Compiled library is missing")?;
                let address =
                    profiling.deploy_library(case.deployer_address(), hex::decode(bytecode)?)?;
                profiling.register_contract(instance.clone(), address, abi.clone());
                input
                    .libraries
                    .entry(metadata.directory()?.join(path).canonicalize()?)
                    .or_default()
                    .insert(name.to_string(), address);
                compiled = compile(compiler.as_ref(), &input, &context).await?;
            }
        }
        profiling
            .run(metadata, case, &compiled, compiler.version())
            .context("Workload profiling failed")?;
        let mut function_names = BTreeMap::<String, BTreeSet<String>>::new();
        for source in sources.values() {
            let (_, abi) = compiled
                .contracts
                .get(&source.contract_source_path)
                .and_then(|contracts| contracts.get(source.contract_ident.as_str()))
                .context("Compiled contract missing from selector lookup")?;
            for function in abi.functions() {
                function_names
                    .entry(hex::encode(function.selector()))
                    .or_default()
                    .insert(format!(
                        "{}.{}",
                        source.contract_ident,
                        function.signature()
                    ));
            }
        }
        report.workloads.push(WorkloadProfilingReport {
            metadata_file_path: metadata.metadata_file_path.clone(),
            case_index,
            mode: mode.into_owned(),
            name: case.name.clone(),
            function_names,
            deployments: profiling
                .deployments
                .into_iter()
                .map(|(address, instance)| {
                    let source = sources
                        .get(&instance)
                        .context("Deployed contract has no source")?;
                    Ok((address, source.contract_ident.to_string()))
                })
                .collect::<Result<_>>()?,
            transactions: profiling.transactions,
        });
        info!("Workload profiling finished");
    }
    ensure!(
        !report.workloads.is_empty(),
        "No compatible EVM workloads matched the selection"
    );
    let path = report.write(&context.working_directory.working_directory)?;
    info!(path = %path.display(), "Profiling report has been written");
    Ok(())
}

async fn compile(
    compiler: &dyn ContractCompiler,
    input: &CompilerInput,
    context: &Profile,
) -> Result<CompilerOutput> {
    let cache = context
        .working_directory
        .working_directory
        .join("profiling_compilations");
    let key = hex::encode(sp_io::hashing::blake2_256(&serde_json::to_vec(&json!({
        "compiler": compiler.fingerprint(), "input": input,
    }))?));
    if !context.compilation.invalidate_cache
        && let Ok(bytes) = cacache::read(&cache, &key).await
    {
        return serde_json::from_slice(&bytes).context("Invalid cached compilation");
    }
    let output = compiler.build(input.clone()).await?;
    cacache::write(cache, key, serde_json::to_vec(&output)?).await?;
    Ok(output)
}
