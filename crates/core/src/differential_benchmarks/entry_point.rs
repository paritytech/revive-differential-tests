//! The main entry point for differential benchmarking.

use crate::internal_prelude::*;

use crate::interpreter::Interpreter;

/// Handles the differential testing executing it according to the information defined in the
/// context
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_differential_benchmarks(
    mut context: Benchmark,
    reporter: Reporter,
) -> anyhow::Result<()> {
    // A bit of a hack but we need to override the number of nodes specified through the CLI since
    // benchmarks can only be run on a single node. Perhaps in the future we'd have a cleaner way to
    // do this. But, for the time being, we need to override the cli arguments.
    if context.concurrency.number_of_nodes != 1 {
        warn!(
            specified_number_of_nodes = context.concurrency.number_of_nodes,
            updated_number_of_nodes = 1,
            "Invalid number of nodes specified through the CLI. Benchmarks can only be run on a single node. Updated the arguments."
        );
        context.concurrency.number_of_nodes = 1;
    };
    let full_context = Context::Benchmark(Box::new(context.clone()));

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

    // Starting the nodes of the various platforms specified in the context. Note that we use the
    // node pool since it contains all of the code needed to spawn nodes from A to Z and therefore
    // it's the preferred way for us to start nodes even when we're starting just a single node. The
    // added overhead from it is quite small (performance wise) since it's involved only when we're
    // creating the test definitions, but it might have other maintenance overhead as it obscures
    // the fact that only a single node is spawned.
    let platforms_and_nodes = {
        let mut map = BTreeMap::new();

        for platform in platforms.iter() {
            let platform_identifier = platform.platform_identifier();

            let node_pool = NodePool::new(full_context.clone(), *platform)
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

    // Preparing test definitions for the execution.
    let allowed_modes = ModeAllowList::from_parsed_modes(context.corpus.allowed_modes.iter());
    let test_definitions = create_test_definitions_stream(
        &full_context,
        &corpus,
        &platforms_and_nodes,
        &allowed_modes,
        &Default::default(),
        reporter.clone(),
    )
    .await
    .collect::<Vec<_>>()
    .await;
    info!(len = test_definitions.len(), "Created test definitions");

    // Creating the objects that will be shared between the various runs. The cached compiler is the
    // only one at the current moment of time that's safe to share between runs.
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

    // Note: we do not want to run all of the workloads concurrently on all platforms. Rather, we'd
    // like to run all of the workloads for one platform, and then the next sequentially as we'd
    // like for the effect of concurrency to be minimized when we're doing the benchmarking.
    for platform in platforms.iter() {
        let platform_identifier = platform.platform_identifier();

        let span = info_span!("Benchmarking for the platform", %platform_identifier);
        let _guard = span.enter();

        let private_key_allocator = Arc::new(Mutex::new(PrivateKeyAllocator::new(
            context.wallet.highest_private_key_exclusive(),
        )));

        for test_definition in test_definitions.iter() {
            let platform_information = &test_definition.platforms[&platform_identifier];

            let span = info_span!(
                "Executing workload",
                metadata_file_path = %test_definition.metadata_file_path.display(),
                case_idx = %test_definition.case_idx,
                mode = %test_definition.mode,
            );
            let _guard = span.enter();

            // Initializing all of the components requires to execute this particular workload.
            let private_key_allocator = private_key_allocator.clone();
            let (watcher, watcher_tx) = Watcher::new(
                platform_information.connector.clone(),
                test_definition
                    .reporter
                    .execution_specific_reporter(0usize, platform_identifier),
            );
            let steps = test_definition
                .case
                .steps_iterator_for_benchmarks(context.benchmark_run.default_repetition_count)
                .enumerate()
                .map(|(step_idx, step)| -> (StepPath, Step) {
                    (StepPath::new(vec![StepIdx::new(step_idx)]), step)
                })
                .collect::<Vec<_>>()
                .into_iter();
            let interpreter = Interpreter::for_benchmarks(
                platform_information,
                test_definition,
                private_key_allocator,
                watcher_tx,
                cached_compiler.as_ref(),
                steps,
            )
            .await
            .context("Failed to create the benchmarks interpreter")?
            .with_repetition_count_override(context.benchmark_run.repetition_count_override);

            let watcher_task = tokio::spawn(watcher.run().instrument(info_span!(
                "Running Watcher",
                %platform_identifier,
                case_name = %test_definition.case.name.clone().unwrap_or_default()
            )))
            .map(|rtn| rtn.context("Watcher failed").flatten());
            let interpreter_task = interpreter
                .run_to_completion()
                .instrument(info_span!(
                    "Executing Benchmarks",
                    %platform_identifier,
                    case_name = %test_definition.case.name.clone().unwrap_or_default()
                ))
                .inspect_ok(|_| info!("Workload Execution Succeeded"))
                .inspect_err(|err| error!(?err, "Workload Execution Failed"))
                .map(|rtn| rtn.context("Failed to run the interpreter and executor"));

            let rtn = try_join(interpreter_task, watcher_task)
                .await
                .context("Failure in benchmarks");

            if context.shutdown.keep_alive_on_failures && rtn.is_err() {
                error!("One of the critical tasks failed, keeping the tool alive to investigate");
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }

            rtn.context("Benchmark run failed")?;
        }
    }

    if context.shutdown.keep_alive {
        info!("Done, keeping framework alive");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    Ok(())
}
