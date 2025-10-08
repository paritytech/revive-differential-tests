use anyhow::Context;
use clap::Parser;
use revive_dt_common::{
    iterators::FilesWithExtensionIterator,
    types::{PlatformIdentifier, PrivateKeyAllocator},
};
use revive_dt_config::TestExecutionContext;
use revive_dt_core::{
    CachedCompiler, Platform,
    helpers::{TestDefinition, TestPlatformInformation},
};
use revive_dt_format::{
    case::CaseIdx,
    corpus::Corpus,
    metadata::{Metadata, MetadataFile},
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use temp_dir::TempDir;
use tokio::sync::Mutex;
use tracing::info;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

/// ML-based test runner for executing differential tests file by file
#[derive(Debug, Parser)]
#[command(name = "ml-test-runner")]
struct MlTestRunnerArgs {
    /// Path to test file (.sol), corpus file (.json), or folder containing .sol files
    #[arg(value_name = "PATH")]
    path: PathBuf,

    /// File to cache tests that have already passed
    #[arg(long = "cached-passed")]
    cached_passed: Option<PathBuf>,

    /// Stop after the first file failure
    #[arg(long = "bail")]
    bail: bool,

    /// Platform to test against (e.g., geth-evm-solc, kitchensink-polkavm-resolc)
    #[arg(long = "platform", default_value = "geth-evm-solc")]
    platform: PlatformIdentifier,

    /// Start the platform and wait for RPC readiness
    #[arg(long = "start-platform", default_value = "false")]
    start_platform: bool,

    /// Private key to use for wallet initialization (hex string with or without 0x prefix)
    #[arg(
        long = "private-key",
        default_value = "0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133"
    )]
    private_key: String,

    /// RPC port to connect to when using existing node
    #[arg(long = "rpc-port", default_value = "8545")]
    rpc_port: u16,
}

fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    let args = MlTestRunnerArgs::parse();

    info!("ML test runner starting");
    info!("Platform: {:?}", args.platform);
    info!("Start platform: {}", args.start_platform);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime")
        .block_on(run(args))
}

async fn run(args: MlTestRunnerArgs) -> anyhow::Result<()> {
    let start_time = Instant::now();

    info!("Discovering test files from: {}", args.path.display());
    let test_files = discover_test_files(&args.path)?;
    info!("Found {} test file(s)", test_files.len());

    let cached_passed = if let Some(cache_file) = &args.cached_passed {
        let cached = load_cached_passed(cache_file)?;
        info!("Loaded {} cached passed test(s)", cached.len());
        cached
    } else {
        HashSet::new()
    };

    let cached_passed = Arc::new(Mutex::new(cached_passed));

    let mut passed_files = 0;
    let mut failed_files = 0;
    let mut skipped_files = 0;
    let mut failures = Vec::new();

    const GREEN: &str = "\x1B[32m";
    const RED: &str = "\x1B[31m";
    const YELLOW: &str = "\x1B[33m";
    const COLOUR_RESET: &str = "\x1B[0m";
    const BOLD: &str = "\x1B[1m";
    const BOLD_RESET: &str = "\x1B[22m";

    for test_file in test_files {
        let file_display = test_file.display().to_string();

        // Check if already passed
        {
            let cache = cached_passed.lock().await;
            if cache.contains(&file_display) {
                println!("test {} ... {YELLOW}cached{COLOUR_RESET}", file_display);
                skipped_files += 1;
                continue;
            }
        }

        info!("Loading metadata from: {}", test_file.display());
        let metadata_file = match load_metadata_file(&test_file) {
            Ok(mf) => {
                info!("Loaded metadata with {} case(s)", mf.cases.len());
                mf
            }
            Err(e) => {
                println!("test {} ... {RED}FAILED{COLOUR_RESET}", file_display);
                println!("    Error loading metadata: {}", e);
                failed_files += 1;
                failures.push((
                    file_display.clone(),
                    format!("Error loading metadata: {}", e),
                ));
                if args.bail {
                    break;
                }
                continue;
            }
        };

        info!("Executing test file: {}", file_display);
        match execute_test_file(&args, &metadata_file).await {
            Ok(_) => {
                println!("test {} ... {GREEN}ok{COLOUR_RESET}", file_display);
                info!("Test file passed: {}", file_display);
                passed_files += 1;

                {
                    let mut cache = cached_passed.lock().await;
                    cache.insert(file_display);
                }
            }
            Err(e) => {
                println!("test {} ... {RED}FAILED{COLOUR_RESET}", file_display);
                failed_files += 1;
                failures.push((file_display, format!("{:?}", e)));

                if args.bail {
                    info!("Bailing after first failure");
                    break;
                }
            }
        }
    }

    if let Some(cache_file) = &args.cached_passed {
        let cache = cached_passed.lock().await;
        info!("Saving {} cached passed test(s)", cache.len());
        save_cached_passed(cache_file, &cache)?;
    }

    // Print summary
    println!();
    if !failures.is_empty() {
        println!("{BOLD}failures:{BOLD_RESET}");
        println!();
        for (file, error) in &failures {
            println!("---- {} ----", file);
            println!("{}", error);
            println!();
        }
    }

    let elapsed = start_time.elapsed();
    println!(
        "test result: {}. {} passed; {} failed; {} cached; finished in {:.2}s",
        if failed_files == 0 {
            format!("{GREEN}ok{COLOUR_RESET}")
        } else {
            format!("{RED}FAILED{COLOUR_RESET}")
        },
        passed_files,
        failed_files,
        skipped_files,
        elapsed.as_secs_f64()
    );

    if failed_files > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Discover test files from the given path
fn discover_test_files(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    let mut files = Vec::new();

    if path.is_file() {
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

        match extension {
            "sol" => {
                // Single .sol file
                files.push(path.to_path_buf());
            }
            "json" => {
                // Corpus file - enumerate its tests
                let corpus = Corpus::try_from_path(path)?;
                let metadata_files = corpus.enumerate_tests();
                for metadata in metadata_files {
                    files.push(metadata.metadata_file_path);
                }
            }
            _ => anyhow::bail!(
                "Unsupported file extension: {}. Expected .sol or .json",
                extension
            ),
        }
    } else if path.is_dir() {
        // Walk directory recursively for .sol files
        for entry in FilesWithExtensionIterator::new(path)
            .with_allowed_extension("sol")
            .with_use_cached_fs(true)
        {
            files.push(entry);
        }
    } else {
        anyhow::bail!("Path is neither a file nor a directory: {}", path.display());
    }

    Ok(files)
}

/// Load metadata from a test file
fn load_metadata_file(path: &Path) -> anyhow::Result<MetadataFile> {
    let metadata = Metadata::try_from_file(path)
        .ok_or_else(|| anyhow::anyhow!("Failed to load metadata from {}", path.display()))?;

    Ok(MetadataFile {
        metadata_file_path: path.to_path_buf(),
        corpus_file_path: path.to_path_buf(),
        content: metadata,
    })
}

/// Execute all test cases in a metadata file
async fn execute_test_file(
    args: &MlTestRunnerArgs,
    metadata_file: &MetadataFile,
) -> anyhow::Result<()> {
    if metadata_file.cases.is_empty() {
        anyhow::bail!("No test cases found in file");
    }

    info!("Processing {} test case(s)", metadata_file.cases.len());

    // Get the platform based on CLI args
    let platform: &dyn Platform = match args.platform {
        PlatformIdentifier::GethEvmSolc => &revive_dt_core::GethEvmSolcPlatform,
        PlatformIdentifier::LighthouseGethEvmSolc => &revive_dt_core::LighthouseGethEvmSolcPlatform,
        PlatformIdentifier::KitchensinkPolkavmResolc => {
            &revive_dt_core::KitchensinkPolkavmResolcPlatform
        }
        PlatformIdentifier::KitchensinkRevmSolc => &revive_dt_core::KitchensinkRevmSolcPlatform,
        PlatformIdentifier::ReviveDevNodePolkavmResolc => {
            &revive_dt_core::ReviveDevNodePolkavmResolcPlatform
        }
        PlatformIdentifier::ReviveDevNodeRevmSolc => &revive_dt_core::ReviveDevNodeRevmSolcPlatform,
        PlatformIdentifier::ZombienetPolkavmResolc => {
            &revive_dt_core::ZombienetPolkavmResolcPlatform
        }
        PlatformIdentifier::ZombienetRevmSolc => &revive_dt_core::ZombienetRevmSolcPlatform,
    };

    let temp_dir = TempDir::new()?;
    info!("Created temporary directory: {}", temp_dir.path().display());

    let test_context = TestExecutionContext::default();
    let context = revive_dt_config::Context::Test(Box::new(test_context));

    let node: &'static dyn revive_dt_node_interaction::EthereumNode = if args.start_platform {
        info!("Starting blockchain node...");
        let node_handle = platform
            .new_node(context.clone())
            .context("Failed to spawn node thread")?;

        info!("Waiting for node to start...");
        let node = node_handle
            .join()
            .map_err(|e| anyhow::anyhow!("Node thread panicked: {:?}", e))?
            .context("Failed to start node")?;

        info!(
            "Node started with ID: {}, connection: {}",
            node.id(),
            node.connection_string()
        );
        let node = Box::leak(node);

        info!("Running pre-transactions...");
        node.pre_transactions()
            .await
            .context("Failed to run pre-transactions")?;
        info!("Pre-transactions completed");

        node
    } else {
        info!("Using existing node");
        let existing_node: Box<dyn revive_dt_node_interaction::EthereumNode> = match args.platform {
            PlatformIdentifier::GethEvmSolc | PlatformIdentifier::LighthouseGethEvmSolc => {
                Box::new(
                    revive_dt_node::node_implementations::geth::GethNode::new_existing(
                        &args.private_key,
                        args.rpc_port,
                    )
                    .await?,
                )
            }
            PlatformIdentifier::KitchensinkPolkavmResolc
            | PlatformIdentifier::KitchensinkRevmSolc
            | PlatformIdentifier::ReviveDevNodePolkavmResolc
            | PlatformIdentifier::ReviveDevNodeRevmSolc
            | PlatformIdentifier::ZombienetPolkavmResolc
            | PlatformIdentifier::ZombienetRevmSolc => Box::new(
                revive_dt_node::node_implementations::substrate::SubstrateNode::new_existing(
                    &args.private_key,
                    args.rpc_port,
                )
                .await?,
            ),
        };
        Box::leak(existing_node)
    };

    info!("Initializing cached compiler");
    let cached_compiler = CachedCompiler::new(temp_dir.path().join("compilation_cache"), false)
        .await
        .map(Arc::new)
        .context("Failed to create cached compiler")?;

    let private_key_allocator = Arc::new(Mutex::new(PrivateKeyAllocator::new(
        alloy::primitives::U256::from(100),
    )));

    let (reporter, report_task) =
        revive_dt_report::ReportAggregator::new(context.clone()).into_task();

    tokio::spawn(report_task);

    info!(
        "Building test definitions for {} case(s)",
        metadata_file.cases.len()
    );
    let mut test_definitions = Vec::new();
    for (case_idx, case) in metadata_file.cases.iter().enumerate() {
        info!("Building test definition for case {}", case_idx);
        let test_def = build_test_definition(
            metadata_file,
            case,
            case_idx,
            platform,
            node,
            &context,
            &reporter,
        )
        .await?;

        if let Some(test_def) = test_def {
            info!("Test definition for case {} created successfully", case_idx);
            test_definitions.push(test_def);
        }
    }

    info!("Executing {} test definition(s)", test_definitions.len());
    for (idx, test_definition) in test_definitions.iter().enumerate() {
        info!("─────────────────────────────────────────────────────────────────");
        info!(
            "Executing case {}/{}: case_idx={}, mode={}, steps={}",
            idx + 1,
            test_definitions.len(),
            test_definition.case_idx,
            test_definition.mode,
            test_definition.case.steps.len()
        );

        info!("Creating driver for case {}", test_definition.case_idx);
        let driver = revive_dt_core::differential_tests::Driver::new_root(
            test_definition,
            private_key_allocator.clone(),
            &cached_compiler,
        )
        .await
        .context("Failed to create driver")?;

        info!(
            "Running {} step(s) for case {}",
            test_definition.case.steps.len(),
            test_definition.case_idx
        );
        let steps_executed = driver.execute_all().await.context(format!(
            "Failed to execute case {}",
            test_definition.case_idx
        ))?;
        info!(
            "✓ Case {} completed successfully, executed {} step(s)",
            test_definition.case_idx, steps_executed
        );
    }
    info!("─────────────────────────────────────────────────────────────────");
    info!(
        "All {} test case(s) executed successfully",
        test_definitions.len()
    );

    Ok(())
}

/// Build a test definition for a single test case
async fn build_test_definition<'a>(
    metadata_file: &'a MetadataFile,
    case: &'a revive_dt_format::case::Case,
    case_idx: usize,
    platform: &'a dyn Platform,
    node: &'a dyn revive_dt_node_interaction::EthereumNode,
    context: &revive_dt_config::Context,
    reporter: &revive_dt_report::Reporter,
) -> anyhow::Result<Option<TestDefinition<'a>>> {
    let mode = case
        .modes
        .as_ref()
        .or(metadata_file.modes.as_ref())
        .and_then(|modes| modes.first())
        .and_then(|parsed_mode| parsed_mode.to_modes().next())
        .map(Cow::Owned)
        .or_else(|| revive_dt_compiler::Mode::all().next().map(Cow::Borrowed))
        .unwrap();

    let compiler = platform
        .new_compiler(context.clone(), mode.version.clone().map(Into::into))
        .await
        .context("Failed to create compiler")?;

    let test_reporter =
        reporter.test_specific_reporter(Arc::new(revive_dt_report::TestSpecifier {
            solc_mode: mode.as_ref().clone(),
            metadata_file_path: metadata_file.metadata_file_path.clone(),
            case_idx: CaseIdx::new(case_idx),
        }));

    let execution_reporter =
        test_reporter.execution_specific_reporter(node.id(), platform.platform_identifier());

    let mut platforms = BTreeMap::new();
    platforms.insert(
        platform.platform_identifier(),
        TestPlatformInformation {
            platform,
            node,
            compiler,
            reporter: execution_reporter,
        },
    );

    let test_definition = TestDefinition {
        metadata: metadata_file,
        metadata_file_path: &metadata_file.metadata_file_path,
        mode,
        case_idx: CaseIdx::new(case_idx),
        case,
        platforms,
        reporter: test_reporter,
    };

    if let Err((reason, _)) = test_definition.check_compatibility() {
        println!("    Skipping case {}: {}", case_idx, reason);
        return Ok(None);
    }

    Ok(Some(test_definition))
}

/// Load cached passed tests from file
fn load_cached_passed(path: &Path) -> anyhow::Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }

    let file = File::open(path).context("Failed to open cached-passed file")?;
    let reader = BufReader::new(file);

    let mut cache = HashSet::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            cache.insert(trimmed.to_string());
        }
    }

    Ok(cache)
}

/// Save cached passed tests to file
fn save_cached_passed(path: &Path, cache: &HashSet<String>) -> anyhow::Result<()> {
    let file = File::create(path).context("Failed to create cached-passed file")?;
    let mut writer = BufWriter::new(file);

    let mut entries: Vec<_> = cache.iter().collect();
    entries.sort();

    for entry in entries {
        writeln!(writer, "{}", entry)?;
    }

    writer.flush()?;
    Ok(())
}
