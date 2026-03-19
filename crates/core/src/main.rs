mod compilations;
mod differential_benchmarks;
mod differential_tests;
mod helpers;

#[allow(unused_imports)]
mod internal_prelude {
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_compiler::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_core::prelude::*;
    pub use revive_dt_format::prelude::*;
    pub use revive_dt_node::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;
    pub use revive_dt_report::prelude::*;

    pub use std::borrow::Cow;
    pub use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    pub use std::future::Future;
    pub use std::io::{BufWriter, Write, stderr};
    pub use std::ops::ControlFlow;
    pub use std::path::{Path, PathBuf};
    pub use std::pin::Pin;
    pub use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    pub use std::sync::{Arc, LazyLock, Mutex as StdMutex};
    pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub use alloy::consensus::EMPTY_ROOT_HASH;
    pub use alloy::hex::{self, ToHexExt};
    pub use alloy::json_abi::JsonAbi;
    pub use alloy::network::{Ethereum, TransactionBuilder};
    pub use alloy::primitives::{Address, BlockNumber, TxHash, U256, address};
    pub use alloy::providers::{DynProvider, PendingTransactionBuilder, Provider};
    pub use alloy::rpc::types::trace::geth::{
        CallFrame, GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType,
        GethDebugTracingOptions,
    };
    pub use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
    pub use ansi_term::{ANSIStrings, Color};
    pub use anyhow::Context as _;
    pub use anyhow::{Error, Result, bail};
    pub use clap::Parser;
    pub use futures::future::{Either, try_join, try_join_all, try_join3};
    pub use futures::{FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt, stream};
    pub use indexmap::{IndexMap, indexmap};
    pub use pallet_revive_eth_rpc::ReceiptExtractor;
    pub use regex::Regex;
    pub use schemars::schema_for;
    pub use semver::{Version, VersionReq};
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{self, Value, json};
    pub use subxt::tx::Payload;
    pub use subxt::utils::H256;
    pub use subxt::{OnlineClient, PolkadotConfig};
    pub use tokio::select;
    pub use tokio::sync::broadcast;
    pub use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
    pub use tokio::sync::{Mutex, Notify, OnceCell, RwLock, Semaphore};
    pub use tokio::time::{interval, timeout};
    pub use tracing::level_filters::LevelFilter;
    pub use tracing::{
        Instrument, Span, debug, debug_span, error, field::display, info, info_span, instrument,
        warn,
    };
    pub use tracing_appender::non_blocking::WorkerGuard;
    pub use tracing_subscriber::EnvFilter;

    pub use crate::compilations::handle_compilations;
    pub use crate::differential_benchmarks::{
        InclusionWatcher, Watcher, WatcherEvent, handle_differential_benchmarks,
    };
    pub use crate::differential_tests::handle_differential_tests;
    pub use crate::helpers::{
        CachedCompiler, CompilationDefinition, CorpusDefinitionProcessor, NodePool,
        TestCaseIgnoreResolvedConfiguration, TestDefinition, TestPlatformInformation,
        create_compilation_definitions_stream, create_test_definitions_stream, process_corpus,
    };
}

use crate::internal_prelude::*;

#[cfg(feature = "tokio-debug")]
fn setup_tracing(_: &LogConfiguration) -> Result<()> {
    console_subscriber::init();
    Ok(())
}

#[cfg(not(feature = "tokio-debug"))]
fn setup_tracing(log_config: &LogConfiguration) -> Result<WorkerGuard> {
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        // Assuming that each line contains 255 characters and that each character is one byte, then
        // this means that our buffer is about 4GBs large.
        .buffered_lines_limit(0x1000000)
        .thread_name("buffered writer")
        .finish(std::io::stdout());

    let fmt_layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> = match log_config.log_format
    {
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_ansi(false)
                .json(),
        ),
        LogFormat::Pretty => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_ansi(true)
                .pretty(),
        ),
    };

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::OFF.into())
        .from_env_lossy();

    use tracing_subscriber::layer::SubscriberExt;
    let registry = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(env_filter);

    tracing::subscriber::set_global_default(registry)?;

    Ok(guard)
}

fn main() -> anyhow::Result<()> {
    let mut context = Context::try_parse()?;
    context.update_for_profile();

    let _guard = setup_tracing(context.as_log_configuration())?;

    info!("Differential testing tool is starting");

    let (reporter, report_aggregator_task) = ReportAggregator::new(context.clone()).into_task();

    match context {
        Context::Test(context) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.concurrency.number_of_threads)
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

                let contains_failure = report
                    .execution_information
                    .values()
                    .flat_map(|values| values.case_reports.values())
                    .flat_map(|values| values.mode_execution_reports.values())
                    .any(|report| matches!(report.status, Some(TestCaseStatus::Failed { .. })));

                if contains_failure {
                    bail!("Some tests failed")
                }

                Ok(())
            }),
        Context::Benchmark(context) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.concurrency.number_of_threads)
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

                let contains_failure = report
                    .execution_information
                    .values()
                    .flat_map(|values| values.case_reports.values())
                    .flat_map(|values| values.mode_execution_reports.values())
                    .any(|report| matches!(report.status, Some(TestCaseStatus::Failed { .. })));

                if contains_failure {
                    bail!("Some benchmarks failed")
                }

                Ok(())
            }),
        Context::ExportGenesis(ref export_genesis_context) => {
            let platform = Into::<&dyn Platform>::into(export_genesis_context.target.platform);
            let genesis = platform.export_genesis(context)?;
            let genesis_json = serde_json::to_string_pretty(&genesis)
                .context("Failed to serialize the genesis to JSON")?;
            println!("{genesis_json}");

            Ok(())
        }
        Context::ExportJsonSchema(_) => {
            let schema = schema_for!(Metadata);
            println!(
                "{}",
                serde_json::to_string_pretty(&schema)
                    .context("Failed to export the JSON schema")?
            );

            Ok(())
        }
        Context::Compile(context) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.concurrency.number_of_threads)
            .enable_all()
            .build()
            .expect("Failed building the Runtime")
            .block_on(async move {
                let compilations_handling_task = handle_compilations(*context, reporter);

                let (_, report) =
                    futures::future::try_join(compilations_handling_task, report_aggregator_task)
                        .await?;

                let contains_failure = report
                    .execution_information
                    .values()
                    .flat_map(|metadata_file_report| {
                        metadata_file_report.compilation_reports.values()
                    })
                    .any(|report| matches!(report.status, Some(CompilationStatus::Failure { .. })));

                if contains_failure {
                    bail!("Some compilations failed")
                }

                Ok(())
            }),
        Context::ExportTestSpecifiers(context) => {
            context
                .corpus
                .test_specifiers
                .into_iter()
                .try_fold(Corpus::new(), Corpus::with_test_specifier)?
                .cases_iterator()
                .for_each(|(metadata, case_idx, _, mode)| {
                    println!(
                        "{}::{}::{}",
                        metadata.metadata_file_path.display(),
                        case_idx,
                        mode
                    )
                });
            Ok(())
        }
    }
}
