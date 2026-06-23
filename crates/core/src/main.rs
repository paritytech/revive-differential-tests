mod compilations;
mod differential_benchmarks;
mod differential_tests;
mod helpers;
mod interpreter;

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

    pub use std::{
        borrow::Cow,
        collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
        future::{Future, ready},
        io::{BufWriter, Write, stderr},
        ops::ControlFlow,
        path::{Path, PathBuf},
        pin::Pin,
        sync::{
            Arc, LazyLock, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    pub use alloy::{
        consensus::EMPTY_ROOT_HASH,
        hex::{self, ToHexExt},
        json_abi::{Function, JsonAbi},
        network::{Ethereum, TransactionBuilder},
        primitives::{Address, B256, BlockNumber, Bytes, Log, TxHash, U256, address},
        providers::{DynProvider, PendingTransactionBuilder, Provider},
        rpc::types::{
            TransactionReceipt, TransactionRequest,
            trace::geth::{
                CallFrame, CallLogFrame, GethDebugBuiltInTracerType, GethDebugTracerConfig,
                GethDebugTracerType, GethDebugTracingOptions,
            },
        },
    };
    pub use ansi_term::{ANSIStrings, Color};
    pub use anyhow::{Context as _, Error, Result, anyhow, bail, ensure};
    pub use clap::Parser;
    pub use dashmap::DashMap;
    pub use futures::{
        FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt,
        future::{Either, join_all, try_join, try_join_all, try_join3},
        stream,
    };
    pub use indexmap::{IndexMap, IndexSet, indexmap};
    pub use regex::Regex;
    pub use schemars::schema_for;
    pub use semver::{Version, VersionReq};
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{self, Value, json};
    pub use subxt::{
        OnlineClient, PolkadotConfig, backend::rpc::RpcClient, tx::Payload, utils::H256,
    };
    pub use tokio::{
        select,
        sync::{
            Mutex, Notify, OnceCell, RwLock, Semaphore, broadcast,
            mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
        },
        task::JoinHandle,
        time::{interval, timeout},
    };
    pub use tracing::{
        Instrument, Span, debug, debug_span, error, field::display, info, info_span, instrument,
        level_filters::LevelFilter, trace, warn,
    };
    pub use tracing_appender::non_blocking::WorkerGuard;
    pub use tracing_subscriber::EnvFilter;

    pub use crate::{
        compilations::handle_compilations,
        differential_benchmarks::{Watcher, WatcherEvent, handle_differential_benchmarks},
        differential_tests::handle_differential_tests,
        helpers::{
            CachedCompiler, CompilationDefinition, CorpusDefinitionProcessor, NodePool,
            TestCaseIgnoreResolvedConfiguration, TestDefinition, TestPlatformInformation,
            create_compilation_definitions_stream, create_test_definitions_stream, process_corpus,
        },
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

    // The `tokio-debug` variant returns `()`, but the default variant returns a `WorkerGuard`
    // that must be held until `main` exits to keep the non-blocking tracing writer alive.
    #[allow(clippy::let_unit_value)]
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
            let corpus = context
                .corpus
                .test_specifiers
                .into_iter()
                .try_fold(Corpus::new(), Corpus::with_test_specifier)?;
            let common_prefix = common_directory_prefix(
                corpus
                    .cases_iterator()
                    .map(|(metadata, _, _, _)| metadata.metadata_file_path.as_path()),
            );
            let test_specifiers = corpus
                .cases_iterator()
                .map(|(metadata, case_idx, _, mode)| {
                    let path =
                        remove_directory_prefix(&metadata.metadata_file_path, &common_prefix);
                    let name = format!("{} (Case {} - Mode {})", path.display(), case_idx, mode);
                    let specifier = format!(
                        "{}::{}::{}",
                        metadata.metadata_file_path.display(),
                        case_idx,
                        mode
                    );

                    json!({
                        "name": name,
                        "specifier": specifier,
                    })
                })
                .collect::<Vec<_>>();
            let test_specifiers_json = serde_json::to_string(&test_specifiers)
                .context("Failed to serialize the test specifiers to JSON")?;

            println!("{test_specifiers_json}");
            Ok(())
        }
    }
}

fn common_directory_prefix<'a>(paths: impl IntoIterator<Item = &'a Path>) -> PathBuf {
    let mut parents = paths.into_iter().filter_map(|path| path.parent());
    let Some(first_parent) = parents.next() else {
        return PathBuf::new();
    };

    let mut prefix = first_parent.to_path_buf();
    for parent in parents {
        while !parent.starts_with(&prefix) {
            if !prefix.pop() {
                return PathBuf::new();
            }
        }
    }

    prefix
}

fn remove_directory_prefix<'a>(path: &'a Path, prefix: &Path) -> Cow<'a, Path> {
    if prefix.as_os_str().is_empty() {
        return Cow::Borrowed(path);
    }

    match path.strip_prefix(prefix) {
        Ok(path) if !path.as_os_str().is_empty() => Cow::Borrowed(path),
        Ok(_) | Err(_) => Cow::Borrowed(path),
    }
}
