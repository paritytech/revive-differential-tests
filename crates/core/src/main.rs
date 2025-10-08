mod differential_benchmarks;
mod differential_tests;
mod helpers;

use clap::Parser;
use revive_dt_report::ReportAggregator;
use schemars::schema_for;
use tracing::info;
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
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .pretty()
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    info!("Differential testing tool is starting");

    let context = Context::try_parse()?;
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

                futures::future::try_join(differential_tests_handling_task, report_aggregator_task)
                    .await?;

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

                futures::future::try_join(
                    differential_benchmarks_handling_task,
                    report_aggregator_task,
                )
                .await?;

                Ok(())
            }),
        Context::ExportJsonSchema => {
            let schema = schema_for!(Metadata);
            println!("{}", serde_json::to_string_pretty(&schema).unwrap());
            Ok(())
        }
    }
}
