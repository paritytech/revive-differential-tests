//! The main entry point into compiling in standalone mode without any test execution.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::Context as _;
use futures::{FutureExt, StreamExt};
use revive_dt_compiler::{Mode, ModeOptimizerSetting, ModePipeline};
use revive_dt_format::corpus::Corpus;
use tokio::sync::{RwLock, Semaphore};
use tracing::{Instrument, error, info, info_span, instrument};

use revive_dt_config::{Context, OutputFormat, StandaloneCompilationContext};
use revive_dt_report::Reporter;

use crate::{
    compilations::Driver,
    helpers::{CachedCompiler, create_compilation_definitions_stream},
};

/// Handles the compilations according to the information defined in the context.
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_compilations(
    context: StandaloneCompilationContext,
    reporter: Reporter,
) -> anyhow::Result<()> {
    let reporter_clone = reporter.clone();

    // Discover all of the metadata files that are defined in the context.
    let corpus = context
        .corpus_configuration
        .compilation_specifiers
        .clone()
        .into_iter()
        .try_fold(Corpus::default(), Corpus::with_compilation_specifier)
        .context("Failed to parse the compile corpus")?;
    info!(
        len = corpus.metadata_file_count(),
        "Discovered metadata files"
    );

    let full_context = Context::Compile(Box::new(context.clone()));
    let compilation_definitions = create_compilation_definitions_stream(
        &full_context,
        &corpus,
        // TODO (temporarily always using `z`): Accept mode(s) via CLI.
        Mode {
            pipeline: ModePipeline::ViaYulIR,
            optimize_setting: ModeOptimizerSetting::Mz,
            version: None,
        },
        reporter.clone(),
    )
    .await
    .collect::<Vec<_>>()
    .await;
    info!(
        len = compilation_definitions.len(),
        "Created compilation definitions"
    );

    let cached_compiler = CachedCompiler::new(
        context
            .working_directory
            .as_path()
            .join("compilation_cache"),
        context
            .compilation_configuration
            .invalidate_compilation_cache,
    )
    .await
    .map(Arc::new)
    .context("Failed to initialize cached compiler")?;

    // Creating the driver and compiling all of the contracts.
    let semaphore = context
        .concurrency_configuration
        .concurrency_limit()
        .map(Semaphore::new)
        .map(Arc::new);
    let running_task_list = Arc::new(RwLock::new(BTreeSet::<usize>::new()));
    let driver_task = futures::future::join_all(compilation_definitions.iter().enumerate().map(
        |(compilation_id, compilation_definition)| {
            let running_task_list = running_task_list.clone();
            let semaphore = semaphore.clone();

            let cached_compiler = cached_compiler.clone();
            let mode = compilation_definition.mode.clone();
            let span = info_span!(
                "Compiling Related Files",
                compilation_id,
                metadata_file_path = %compilation_definition.metadata_file_path.display(),
                mode = %mode,
            );
            async move {
                let permit = match semaphore.as_ref() {
                    Some(semaphore) => Some(semaphore.acquire().await.expect("Can't fail")),
                    None => None,
                };

                running_task_list.write().await.insert(compilation_id);

                let driver = Driver::new(compilation_definition);
                match driver.compile_all(&cached_compiler).await {
                    Ok(()) => { /* Reporting already happens by the cached compiler. */ }
                    Err(_) => {
                        /* Reporting already happens by the cached compiler. */
                        error!("Compilation Failed");
                    }
                };
                info!("Finished the compilation of the contracts");
                drop(permit);
                running_task_list.write().await.remove(&compilation_id);
            }
            .instrument(span)
        },
    ))
    .inspect(|_| {
        info!("Finished compiling all contracts");
        reporter_clone
            .report_completion_event()
            .expect("Can't fail")
    });

    let cli_reporting_task = start_cli_reporting_task(context.output_format, reporter);

    tokio::task::spawn(async move {
        loop {
            let remaining_tasks = running_task_list.read().await;
            info!(
                count = remaining_tasks.len(),
                ?remaining_tasks,
                "Remaining Tasks"
            );
            drop(remaining_tasks);
            tokio::time::sleep(Duration::from_secs(10)).await
        }
    });

    futures::future::join(driver_task, cli_reporting_task).await;

    Ok(())
}

// TODO: UPDATE!
#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(output_format: OutputFormat, reporter: Reporter) {
    todo!()
}
