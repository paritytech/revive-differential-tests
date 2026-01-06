mod differential_benchmarks;
mod differential_tests;
mod helpers;

use anyhow::{Context as _, bail};
use clap::Parser;
use revive_dt_common::types::ParsedTestSpecifier;
use revive_dt_report::{ReportAggregator, TestCaseStatus};
use schemars::schema_for;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use revive_dt_config::Context;
use revive_dt_core::Platform;
use revive_dt_format::metadata::Metadata;

use crate::{
    differential_benchmarks::handle_differential_benchmarks,
    differential_tests::handle_differential_tests,
};

fn main() -> anyhow::Result<()> {
    let (writer, _guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
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
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::OFF.into())
                .from_env_lossy(),
        )
        .with_ansi(false)
        .pretty()
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    info!("Differential testing tool is starting");

    let mut context = Context::try_parse()?;
    context.update_for_profile();

    let (reporter, report_aggregator_task) = ReportAggregator::new(context.clone()).into_task();

    match context {
        Context::Test(context) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.concurrency_configuration.number_of_threads)
            .enable_all()
            .build()
            .expect("Failed building the Runtime")
            .block_on(async move {
                let differential_tests_handling_task =
                    handle_differential_tests(*context, reporter);

                let (_, report) = futures::future::try_join(
                    differential_tests_handling_task,
                    report_aggregator_task,
                )
                .await?;

                // Error out if there are any failing tests.
                let failures = report
                    .execution_information
                    .into_iter()
                    .flat_map(|(metadata_file_path, metadata_file_report)| {
                        metadata_file_report.case_reports.into_iter().flat_map(
                            move |(case_idx, case_report)| {
                                let metadata_file_path = metadata_file_path.clone();
                                case_report.mode_execution_reports.into_iter().filter_map(
                                    move |(mode, execution_report)| {
                                        if let Some(TestCaseStatus::Failed { reason }) =
                                            execution_report.status
                                        {
                                            let parsed_test_specifier =
                                                ParsedTestSpecifier::CaseWithMode {
                                                    metadata_file_path: metadata_file_path
                                                        .clone()
                                                        .into_inner(),
                                                    case_idx: case_idx.into_inner(),
                                                    mode,
                                                };
                                            Some((parsed_test_specifier, reason))
                                        } else {
                                            None
                                        }
                                    },
                                )
                            },
                        )
                    })
                    .collect::<Vec<_>>();

                if !failures.is_empty() {
                    bail!("Some tests failed: {failures:#?}")
                }

                Ok(())
            }),
        Context::Benchmark(context) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.concurrency_configuration.number_of_threads)
            .enable_all()
            .build()
            .expect("Failed building the Runtime")
            .block_on(async move {
                let differential_benchmarks_handling_task =
                    handle_differential_benchmarks(*context, reporter);

                let (_, report) = futures::future::try_join(
                    differential_benchmarks_handling_task,
                    report_aggregator_task,
                )
                .await?;

                // Error out if there are any failing tests.
                let failures = report
                    .execution_information
                    .into_iter()
                    .flat_map(|(metadata_file_path, metadata_file_report)| {
                        metadata_file_report.case_reports.into_iter().flat_map(
                            move |(case_idx, case_report)| {
                                let metadata_file_path = metadata_file_path.clone();
                                case_report.mode_execution_reports.into_iter().filter_map(
                                    move |(mode, execution_report)| {
                                        if let Some(TestCaseStatus::Failed { reason }) =
                                            execution_report.status
                                        {
                                            let parsed_test_specifier =
                                                ParsedTestSpecifier::CaseWithMode {
                                                    metadata_file_path: metadata_file_path
                                                        .clone()
                                                        .into_inner(),
                                                    case_idx: case_idx.into_inner(),
                                                    mode,
                                                };
                                            Some((parsed_test_specifier, reason))
                                        } else {
                                            None
                                        }
                                    },
                                )
                            },
                        )
                    })
                    .collect::<Vec<_>>();

                if !failures.is_empty() {
                    bail!("Some tests failed: {failures:#?}")
                }

                Ok(())
            }),
        Context::ExportGenesis(ref export_genesis_context) => {
            let platform = Into::<&dyn Platform>::into(export_genesis_context.platform);
            let genesis = platform.export_genesis(context)?;
            let genesis_json = serde_json::to_string_pretty(&genesis)
                .context("Failed to serialize the genesis to JSON")?;
            println!("{genesis_json}");

            Ok(())
        }
        Context::ExportJsonSchema => {
            let schema = schema_for!(Metadata);
            println!(
                "{}",
                serde_json::to_string_pretty(&schema)
                    .context("Failed to export the JSON schema")?
            );

            Ok(())
        }
    }
}
