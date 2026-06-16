//! The main entry point for differential benchmarking.

use crate::internal_prelude::*;

use crate::differential_benchmarks::Driver;
use crate::differential_benchmarks::{
    SamplingMode, aggregate_to_summary, build_block_jobs, execute_trace_jobs, sample_watched_txs,
};

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
    let test_definitions = create_test_definitions_stream(
        &full_context,
        &corpus,
        &platforms_and_nodes,
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

    // The inclusion watcher to use for all of the benchmarks.
    let inclusion_watcher = InclusionWatcher::new();

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
            let provider = platform_information
                .node
                .provider()
                .await
                .context("Failed to create the node's provider")?;
            let substrate_provider = match platform_information.node.substrate_provider() {
                Some(future) => Some(
                    future
                        .await
                        .context("Failed to create the substrate provider")?,
                ),
                None => None,
            };
            let transaction_finder =
                TransactionFinder::new(provider.clone(), substrate_provider.clone());
            // Cheap Arc-clone — kept so we can resolve sampled tx hashes to
            // their substrate blocks after the driver consumes the original.
            let transaction_finder_for_profiler = transaction_finder.clone();
            let (watcher, watcher_tx) = Watcher::new(
                platform_information
                    .node
                    .subscribe_to_full_blocks_information()
                    .await
                    .context("Failed to subscribe to full blocks information from the node")?,
                provider,
                substrate_provider,
                test_definition
                    .reporter
                    .execution_specific_reporter(0usize, platform_identifier),
            );
            let driver = Driver::new(
                platform_information,
                test_definition,
                context.benchmark_run.repetition_count_override,
                private_key_allocator,
                cached_compiler.as_ref(),
                watcher_tx.clone(),
                false,
                &inclusion_watcher,
                transaction_finder,
                test_definition
                    .case
                    .steps_iterator_for_benchmarks(context.benchmark_run.default_repetition_count)
                    .enumerate()
                    .map(|(step_idx, step)| -> (StepPath, Step) {
                        (StepPath::new(vec![StepIdx::new(step_idx)]), step)
                    }),
            )
            .await
            .context("Failed to create the benchmarks driver")?;

            // Running the auxiliary tasks
            let watcher_task = tokio::spawn(watcher.run().instrument(info_span!(
                "Running Watcher",
                %platform_identifier,
                case_name = %test_definition.case.name.clone().unwrap_or_default()
            )));
            let inclusion_watcher_task = tokio::spawn(
                inclusion_watcher.run(
                    platform_information
                        .node
                        .subscribe_to_transaction_inclusions()
                        .await
                        .context("Failed to subscribe to full blocks information from the node")?,
                ),
            );

            // Running the driver.
            let driver_rtn = driver
                .execute_all()
                .instrument(info_span!(
                    "Executing Benchmarks",
                    %platform_identifier,
                    case_name = %test_definition.case.name.clone().unwrap_or_default()
                ))
                .inspect(|_| {
                    info!("All transactions submitted - driver completed execution");
                })
                .await
                .context("Failed to run the driver and executor")
                .inspect(|(steps_executed, _)| {
                    info!(steps_executed, "Workload Execution Succeeded")
                })
                .inspect_err(|err| error!(?err, "Workload Execution Failed"));

            // Stopping auxiliary tasks.
            watcher_tx
                .send(WatcherEvent::AllTransactionsSubmitted)
                .unwrap();
            inclusion_watcher.stop();

            let watcher_rtn = watcher_task.await.context("Watcher failed").flatten();
            let inclusion_rtn = inclusion_watcher_task.await;

            info!(
                keep_alive_on_failures = context.shutdown.keep_alive_on_failures,
                driver_rtn = driver_rtn.is_err(),
                watcher_rtn = watcher_rtn.is_err(),
                inclusion_rtn = inclusion_rtn.is_err(),
            );

            if context.shutdown.keep_alive_on_failures
                && (driver_rtn.is_err() || watcher_rtn.is_err() || inclusion_rtn.is_err())
            {
                error!(
                    driver_rtn = ?driver_rtn.err(),
                    watcher_rtn = ?watcher_rtn.err(),
                    inclusion_rtn = ?inclusion_rtn.err(),
                    "One of the critical tasks failed, keeping the tool alive to investigate"
                );
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }

            let _ = driver_rtn.context("Driver failed")?;
            let watcher_outcome = watcher_rtn.context("Watcher failed")?;
            inclusion_rtn.context("Inclusion watcher failed")?;

            if context.benchmark_run.profile_watched_txs {
                let mode = if context.benchmark_run.profile_all {
                    SamplingMode::All
                } else {
                    SamplingMode::Sample(context.benchmark_run.profile_samples_per_step_path)
                };
                let sampled_hashes =
                    sample_watched_txs(&watcher_outcome.transaction_registration_information, mode);
                let samples: Vec<(TxHash, StepPath)> = sampled_hashes
                    .into_iter()
                    .filter_map(|h| {
                        watcher_outcome
                            .transaction_registration_information
                            .get(&h)
                            .map(|(sp, _)| (h, sp.clone()))
                    })
                    .collect();
                let watched_tx_count = watcher_outcome.transaction_registration_information.len();
                let sampled_count = samples.len();

                let jobs = build_block_jobs(&transaction_finder_for_profiler, samples)
                    .await
                    .context("Failed to build profiler trace jobs")?;
                let block_count = jobs.len() as u32;

                let profiles = execute_trace_jobs(
                    platform_information.node,
                    jobs,
                    context.benchmark_run.profile_step_limit,
                    context.benchmark_run.profile_concurrency,
                )
                .await;

                let top_opcode = profiles
                    .iter()
                    .flat_map(|p| p.opcodes.first())
                    .max_by_key(|op| op.total_ref_time)
                    .map(|op| op.op_key.as_string())
                    .unwrap_or_else(|| "<none>".to_string());

                info!(
                    %platform_identifier,
                    watched_tx_count,
                    sampled_count,
                    block_count,
                    profile_count = profiles.len(),
                    top_opcode = %top_opcode,
                    "Profiler: workload complete"
                );

                let summary = aggregate_to_summary(profiles, block_count);
                test_definition
                    .reporter
                    .execution_specific_reporter(0usize, platform_identifier)
                    .report_opcode_profile_completed_event(summary)
                    .context("Failed to send opcode_profile_completed event to reporter")?;
            }
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
