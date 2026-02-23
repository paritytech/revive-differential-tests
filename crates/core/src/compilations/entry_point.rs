//! The main entry point into compiling in standalone mode without any test execution.

use std::{
    collections::BTreeSet,
    io::{BufWriter, Write, stderr},
    sync::Arc,
    time::{Duration, Instant},
};

use ansi_term::{ANSIStrings, Color};
use anyhow::Context as _;
use futures::{FutureExt, StreamExt};
use revive_dt_compiler::{Mode, ModeOptimizerSetting, ModePipeline};
use revive_dt_format::corpus::Corpus;
use tokio::sync::{RwLock, Semaphore, broadcast};
use tracing::{Instrument, error, info, info_span, instrument};

use revive_dt_config::{Context, OutputFormat, StandaloneCompilationContext};
use revive_dt_report::{CompilationStatus, Reporter, ReporterEvent};

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

    // Subscribe early, before stream collection, to capture all events including
    // ignored compilations determined during compatibility checks.
    let aggregator_events_rx = reporter.subscribe().await.expect("Can't fail");

    // Discover all of the metadata files that are defined in the context.
    let corpus = context
        .corpus_configuration
        .compilation_specifiers
        .clone()
        .into_iter()
        .try_fold(Corpus::default(), Corpus::with_compilation_specifier)
        .context("Failed to parse the compilation corpus")?;
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
    drop(reporter);
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

    let cli_reporting_task =
        start_cli_reporting_task(context.output_format, context.verbose, aggregator_events_rx);

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

#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(
    output_format: OutputFormat,
    verbose: bool,
    mut aggregator_events_rx: broadcast::Receiver<ReporterEvent>,
) {
    let start = Instant::now();

    let mut global_success_count = 0;
    let mut global_failure_count = 0;
    let mut global_ignore_count = 0;

    let mut buf = BufWriter::new(stderr());
    while let Ok(event) = aggregator_events_rx.recv().await {
        let ReporterEvent::MetadataFileModeCombinationCompilationCompleted {
            metadata_file_path,
            compilation_status,
        } = event
        else {
            continue;
        };

        match output_format {
            OutputFormat::Legacy => {
                let _ = write!(buf, "{}", metadata_file_path.display());
                for (mode, status) in compilation_status {
                    let _ = write!(buf, "\tMode {}: ", mode);
                    let _ = match &status {
                        CompilationStatus::Success {
                            is_cached,
                            compiled_contracts_info,
                            ..
                        } => {
                            global_success_count += 1;
                            let contract_count: usize = compiled_contracts_info
                                .values()
                                .map(|contracts| contracts.len())
                                .sum();
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Green.bold().paint("Compilation Succeeded"),
                                    Color::Green.paint(format!(
                                        " - Contracts compiled: {}, Cached: {}",
                                        contract_count,
                                        if *is_cached { "yes" } else { "no" }
                                    )),
                                ])
                            )
                        }
                        CompilationStatus::Failure { reason, .. } => {
                            global_failure_count += 1;
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Red.bold().paint("Compilation Failed"),
                                    Color::Red.paint(format!(" - Reason: {}", reason.trim())),
                                ])
                            )
                        }
                        CompilationStatus::Ignored { reason, .. } => {
                            global_ignore_count += 1;
                            writeln!(
                                buf,
                                "{}",
                                ANSIStrings(&[
                                    Color::Yellow.bold().paint("Compilation Ignored"),
                                    Color::Yellow.paint(format!(" - Reason: {}", reason.trim())),
                                ])
                            )
                        }
                    };
                }
                let _ = writeln!(buf);
            }
            OutputFormat::CargoTestLike => {
                let mut success_count = 0;
                let mut failure_count = 0;
                let mut ignored_count = 0;

                for (mode, status) in compilation_status {
                    match &status {
                        CompilationStatus::Success {
                            compiled_contracts_info,
                            ..
                        } => {
                            success_count += 1;
                            global_success_count += 1;
                            let contract_count: usize = compiled_contracts_info
                                .values()
                                .map(|contracts| contracts.len())
                                .sum();

                            if verbose {
                                // Verbose: show header + per-contract lines + summary.
                                writeln!(
                                    buf,
                                    "\t{} {} - {}\n",
                                    Color::Green.paint("Compiling"),
                                    metadata_file_path.display(),
                                    mode
                                )
                                .unwrap();
                                writeln!(buf, "compiling {} contracts", contract_count).unwrap();

                                for (source_path, contracts) in compiled_contracts_info {
                                    for contract_name in contracts.keys() {
                                        writeln!(
                                            buf,
                                            "compile {}::{} ... {}",
                                            source_path.display(),
                                            contract_name,
                                            Color::Green.paint("ok")
                                        )
                                        .unwrap();
                                    }
                                }
                                writeln!(buf).unwrap();

                                writeln!(
                                    buf,
                                    "compile result: {}. {} contracts compiled",
                                    Color::Green.paint("ok"),
                                    contract_count
                                )
                                .unwrap();
                                writeln!(buf).unwrap();
                            } else {
                                // Non-verbose: single line with contract count.
                                writeln!(
                                    buf,
                                    "compile {} ({}) ... {} ({} contracts)",
                                    metadata_file_path.display(),
                                    mode,
                                    Color::Green.paint("ok"),
                                    contract_count
                                )
                                .unwrap();
                            }
                        }
                        CompilationStatus::Failure { reason, .. } => {
                            failure_count += 1;
                            global_failure_count += 1;
                            writeln!(
                                buf,
                                "compile {} ({}) ... {}",
                                metadata_file_path.display(),
                                mode,
                                Color::Red.paint(format!("FAILED, {}", reason.trim()))
                            )
                            .unwrap();
                        }
                        CompilationStatus::Ignored { reason, .. } => {
                            ignored_count += 1;
                            global_ignore_count += 1;
                            writeln!(
                                buf,
                                "compile {} ({}) ... {}",
                                metadata_file_path.display(),
                                mode,
                                Color::Yellow.paint(format!("ignored, {}", reason.trim()))
                            )
                            .unwrap();
                        }
                    }
                }

                let status = if failure_count > 0 {
                    Color::Red.paint("FAILED")
                } else {
                    Color::Green.paint("ok")
                };
                writeln!(
                    buf,
                    "compile result: {}. {} succeeded; {} failed; {} ignored",
                    status, success_count, failure_count, ignored_count,
                )
                .unwrap();
                writeln!(buf).unwrap();

                if aggregator_events_rx.is_empty() {
                    buf = tokio::task::spawn_blocking(move || {
                        buf.flush().unwrap();
                        buf
                    })
                    .await
                    .unwrap();
                }
            }
        }
    }
    info!("Aggregator Broadcast Channel Closed");

    // Summary at the end.
    let total = global_success_count + global_failure_count + global_ignore_count;
    match output_format {
        OutputFormat::Legacy => {
            writeln!(
                buf,
                "{} compilations: {} succeeded, {} failed, {} ignored in {} seconds",
                total,
                Color::Green.paint(global_success_count.to_string()),
                Color::Red.paint(global_failure_count.to_string()),
                Color::Yellow.paint(global_ignore_count.to_string()),
                start.elapsed().as_secs()
            )
            .unwrap();
        }
        OutputFormat::CargoTestLike => {
            writeln!(
                buf,
                "\nrun finished. {} succeeded; {} failed; {} ignored; finished in {}s",
                global_success_count,
                global_failure_count,
                global_ignore_count,
                start.elapsed().as_secs()
            )
            .unwrap();
        }
    }
}
