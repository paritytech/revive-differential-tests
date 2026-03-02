//! The main entry point into compiling in pre-link-only mode without any test execution.

use std::{
    io::{BufWriter, Write, stderr},
    time::Instant,
};

use ansi_term::{ANSIStrings, Color};
use anyhow::Context as _;
use futures::StreamExt;
use indexmap::IndexMap;
use revive_dt_compiler::{Mode, ModeOptimizerLevel, ModeOptimizerSetting, ModePipeline};
use revive_dt_config::{
    Compile, Context, FailFastConfiguration, OutputFormat, OutputFormatConfiguration,
};
use revive_dt_format::corpus::Corpus;
use revive_dt_report::{CompilationStatus, Reporter, ReporterEvent};
use tokio::sync::broadcast;
use tracing::{info, info_span, instrument, warn};

use crate::{
    compilations::Driver,
    helpers::{
        CachedCompiler, CompilationDefinition, CorpusDefinitionProcessor,
        create_compilation_definitions_stream, process_corpus,
    },
};

/// The definition processor for compilations.
struct CompilationDefinitionProcessor;

impl CorpusDefinitionProcessor for CompilationDefinitionProcessor {
    type Definition<'a> = CompilationDefinition<'a>;
    type ProcessResult = ();
    type State = ();

    async fn process_definition<'a>(
        definition: &'a Self::Definition<'a>,
        cached_compiler: &'a CachedCompiler<'a>,
        _state: Self::State,
    ) -> anyhow::Result<Self::ProcessResult> {
        Driver::new(definition).compile_all(cached_compiler).await?;
        Ok(())
    }

    /* `on_success` and `on_failure` use the default no-op implementations as reporting already happens by the cached compiler. */

    fn on_ignored(definition: &Self::Definition<'_>, reason: String) -> anyhow::Result<()> {
        definition
            .reporter
            .report_pre_link_contracts_compilation_ignored_event(reason, IndexMap::new())?;
        Ok(())
    }

    fn create_fail_fast_action(
        definition: &Self::Definition<'_>,
        fail_fast: &FailFastConfiguration,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        fail_fast.fail_fast.then(|| {
            let reporter = definition.reporter.clone();
            Box::new(move || {
                let _ = reporter.report_pre_link_contracts_compilation_ignored_event(
                    "Aborted due to fail-fast".to_string(),
                    IndexMap::new(),
                );
            }) as Box<dyn FnOnce() + Send>
        })
    }

    fn create_span(task_id: usize, definition: &Self::Definition<'_>) -> tracing::Span {
        info_span!(
            "Compiling Related Files",
            compilation_id = task_id,
            metadata_file_path = %definition.metadata_file_path.display(),
            mode = %definition.mode,
        )
    }
}

/// Handles the compilations according to the information defined in the context.
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_compilations(mut context: Compile, reporter: Reporter) -> anyhow::Result<()> {
    if !context.compilation.invalidate_cache {
        warn!(
            "Cache invalidation enabled: The compile subcommand always invalidates cache to avoid incorrect results from different compiler binaries."
        );
        context.compilation.invalidate_cache = true;
    }

    let reporter_clone = reporter.clone();

    // Subscribe early, before stream collection, to capture all events including
    // ignored compilations determined during compatibility checks.
    let aggregator_events_rx = reporter.subscribe().await.expect("Can't fail");

    // Discover all of the metadata files that are defined in the context.
    let corpus = context
        .corpus
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
            optimize_setting: ModeOptimizerSetting {
                solc_optimizer_enabled: true,
                level: ModeOptimizerLevel::Mz,
            },
            solc_version: None,
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
            .working_directory
            .as_path()
            .join("compilation_cache"),
        context.compilation.invalidate_cache,
    )
    .await
    .context("Failed to initialize cached compiler")?;

    let cli_reporting_task = tokio::spawn(start_cli_reporting_task(
        context.output_format.clone(),
        aggregator_events_rx,
    ));

    process_corpus::<CompilationDefinitionProcessor>(
        &compilation_definitions,
        &cached_compiler,
        (),
        &context.concurrency,
        &context.fail_fast,
        reporter_clone,
    )
    .await;

    cli_reporting_task
        .await
        .expect("CLI reporting task panicked");

    Ok(())
}

#[allow(irrefutable_let_patterns, clippy::uninlined_format_args)]
async fn start_cli_reporting_task(
    output_format: OutputFormatConfiguration,
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

        match output_format.output_format {
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

                            if output_format.verbose {
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
    match output_format.output_format {
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
