//! The global configuration used across all revive differential testing crates.

use std::{
    fmt::Display,
    ops::Deref,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, LazyLock, OnceLock},
    time::Duration,
};

use alloy::{
    genesis::Genesis,
    network::EthereumWallet,
    primitives::{B256, FixedBytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Context as _;
use clap::{Parser, ValueEnum, ValueHint};
use revive_dt_common::types::{ParsedTestSpecifier, PlatformIdentifier};
use semver::Version;
use serde::{Deserialize, Serialize, Serializer};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use temp_dir::TempDir;

/// The CLI for the differential testing and benchmarking framework.
#[revive_dt_proc_macros::context(
    context_type_ident = "Context",
    default_derives = "Clone, Debug, Parser, Serialize, Deserialize"
)]
#[command(name = "retester", term_width = 100)]
mod context {
    use super::*;

    /// Executes tests in the MatterLabs format differentially on multiple targets concurrently.
    #[subcommand]
    pub struct Test {
        pub profile: ProfileConfiguration,
        pub output_format: OutputFormatConfiguration,
        pub platforms: PlatformConfiguration,
        pub working_directory: WorkingDirectoryConfiguration,
        pub corpus: CorpusConfiguration,
        pub solc: SolcConfiguration,
        pub resolc: ResolcConfiguration,
        pub polkadot_parachain: PolkadotParachainConfiguration,
        pub geth: GethConfiguration,
        pub kurtosis: KurtosisConfiguration,
        pub revive_dev_node: ReviveDevNodeConfiguration,
        pub polkadot_omnichain_node: PolkadotOmnichainNodeConfiguration,
        pub eth_rpc: EthRpcConfiguration,
        pub genesis: GenesisConfiguration,
        pub wallet: WalletConfiguration,
        pub concurrency: ConcurrencyConfiguration,
        pub compilation: CompilationConfiguration,
        pub report: ReportConfiguration,
        pub ignore: IgnoreCasesConfiguration,
    }

    /// Executes differential benchmarks on various platforms.
    #[subcommand]
    pub struct Benchmark {
        pub profile: ProfileConfiguration,
        pub platforms: PlatformConfiguration,
        pub working_directory: WorkingDirectoryConfiguration,
        pub benchmark_run: BenchmarkRunConfiguration,
        pub corpus: CorpusConfiguration,
        pub solc: SolcConfiguration,
        pub resolc: ResolcConfiguration,
        pub polkadot_parachain: PolkadotParachainConfiguration,
        pub geth: GethConfiguration,
        pub kurtosis: KurtosisConfiguration,
        pub revive_dev_node: ReviveDevNodeConfiguration,
        pub polkadot_omnichain_node: PolkadotOmnichainNodeConfiguration,
        pub eth_rpc: EthRpcConfiguration,
        pub wallet: WalletConfiguration,
        pub concurrency: ConcurrencyConfiguration,
        pub compilation: CompilationConfiguration,
        pub report: ReportConfiguration,
    }

    /// Exports the genesis file of the desired platform.
    #[subcommand]
    pub struct ExportGenesis {
        pub target: ExportGenesisTargetConfiguration,
        pub geth: GethConfiguration,
        pub kurtosis: KurtosisConfiguration,
        pub polkadot_parachain: PolkadotParachainConfiguration,
        pub revive_dev_node: ReviveDevNodeConfiguration,
        pub polkadot_omnichain_node: PolkadotOmnichainNodeConfiguration,
        pub wallet: WalletConfiguration,
    }

    /// Exports the JSON schema of the MatterLabs test format used by the tool.
    #[subcommand]
    pub struct ExportJsonSchema;

    /// Configuration for the commandline profile.
    #[configuration]
    pub struct ProfileConfiguration {
        /// The commandline profile to use. Different profiles change the defaults of the various
        /// cli arguments.
        #[arg(long = "profile", default_value_t = Profile::Default)]
        pub profile: Profile,
    }

    /// Configuration for the output format.
    #[configuration]
    pub struct OutputFormatConfiguration {
        /// The output format to use for the tool's output.
        #[arg(short, long, default_value_t = OutputFormat::CargoTestLike)]
        pub output_format: OutputFormat,
    }

    /// Configuration for the set of platforms.
    #[configuration]
    pub struct PlatformConfiguration {
        /// The set of platforms that the differential tests should run on.
        #[arg(
            short = 'p',
            long = "platform",
            id = "platforms",
            default_values = ["geth-evm-solc", "revive-dev-node-polkavm-resolc"]
        )]
        pub platforms: Vec<PlatformIdentifier>,
    }

    /// Configuration for the working directory.
    #[configuration]
    pub struct WorkingDirectoryConfiguration {
        /// The working directory that the program will use for all of the temporary artifacts
        /// needed at runtime.
        ///
        /// If not specified, then a temporary directory will be created and used by the program
        /// for all temporary artifacts.
        #[clap(short, long = "working-directory", default_value = "", value_hint = ValueHint::DirPath)]
        pub working_directory: WorkingDirectoryPath,
    }

    /// Configuration for benchmark-specific parameters.
    #[configuration(key = "benchmark")]
    pub struct BenchmarkRunConfiguration {
        /// The default repetition count for any workload specified but that doesn't contain a
        /// repeat step.
        #[arg(short = 'r', default_value_t = 1000)]
        pub default_repetition_count: usize,

        /// This transaction controls whether the benchmarking driver should await for
        /// transactions to be included in a block before moving on to the next transaction in
        /// the sequence or not.
        pub await_transaction_inclusion: bool,
    }

    /// Configuration for the export-genesis target platform.
    #[configuration]
    pub struct ExportGenesisTargetConfiguration {
        /// The platform of choice to export the genesis for.
        #[arg(default_value = "geth-evm-solc")]
        pub platform: PlatformIdentifier,
    }

    /// A set of configuration parameters for the corpus files to use for the execution.
    #[serde_with::serde_as]
    #[configuration(key = "corpus")]
    pub struct CorpusConfiguration {
        /// A list of test specifiers for the tests that the tool should run.
        ///
        /// Test specifiers follow the following format:
        ///
        /// - `{directory_path|metadata_file_path}`: A path to a metadata file where all of the
        ///   cases live and should be run. Alternatively, it points to a directory instructing the
        ///   framework to discover of the metadata files that live there an execute them.
        /// - `{metadata_file_path}::{case_idx}`: The path to a metadata file and then a case idx
        ///   separated by two colons. This specifies that only this specific test case within the
        ///   metadata file should be executed.
        /// - `{metadata_file_path}::{case_idx}::{mode}`: This is very similar to the above
        ///   specifier with the exception that in this case the mode is specified and will be used
        ///   in the test.
        #[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
        #[arg(short = 't', long = "test", required = true)]
        pub test_specifiers: Vec<ParsedTestSpecifier>,
    }

    /// A set of configuration parameters for Solc.
    #[configuration(key = "solc")]
    pub struct SolcConfiguration {
        /// Specifies the default version of the Solc compiler that should be used if there is no
        /// override specified by one of the test cases.
        #[clap(default_value = "0.8.29")]
        pub version: Version,
    }

    /// A set of configuration parameters for Resolc.
    #[configuration(key = "resolc")]
    pub struct ResolcConfiguration {
        /// Specifies the path of the resolc compiler to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the resolc binary
        /// that's provided in the user's $PATH.
        #[clap(default_value = "resolc")]
        pub path: PathBuf,

        /// Specifies the PVM heap size in bytes.
        ///
        /// If unspecified, the revive compiler default is used
        pub heap_size: Option<u32>,

        /// Specifies the PVM stack size in bytes.
        ///
        /// If unspecified, the revive compiler default is used
        pub stack_size: Option<u32>,
    }

    /// A set of configuration parameters for Polkadot Parachain.
    #[configuration(key = "polkadot-parachain")]
    pub struct PolkadotParachainConfiguration {
        /// Specifies the path of the polkadot-parachain node to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the
        /// polkadot-parachain binary that's provided in the user's $PATH.
        #[clap(default_value = "polkadot-parachain")]
        pub path: PathBuf,

        /// The amount of time to wait upon startup before considering that the node timed out.
        #[clap(default_value = "5000", value_parser = parse_duration)]
        pub start_timeout_ms: Duration,
    }

    /// A set of configuration parameters for Geth.
    #[configuration(key = "geth")]
    pub struct GethConfiguration {
        /// Specifies the path of the geth node to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the geth binary
        /// that's provided in the user's $PATH.
        #[clap(default_value = "geth")]
        pub path: PathBuf,

        /// The amount of time to wait upon startup before considering that the node timed out.
        #[clap(default_value = "30000", value_parser = parse_duration)]
        pub start_timeout_ms: Duration,

        /// The logging configuration to pass to the binary when it's being started.
        #[clap(default_value = "3")]
        pub logging_level: String,
    }

    /// A set of configuration parameters for kurtosis.
    #[configuration(key = "kurtosis")]
    pub struct KurtosisConfiguration {
        /// Specifies the path of the kurtosis node to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the kurtosis binary
        /// that's provided in the user's $PATH.
        #[clap(default_value = "kurtosis")]
        pub path: PathBuf,
    }

    /// A set of configuration parameters for the revive dev node.
    #[configuration(key = "revive-dev-node")]
    pub struct ReviveDevNodeConfiguration {
        /// Specifies the path of the revive dev node to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the revive dev node
        /// binary that's provided in the user's $PATH.
        #[clap(default_value = "revive-dev-node")]
        pub path: PathBuf,

        /// The amount of time to wait upon startup before considering that the node timed out.
        #[clap(default_value = "30000", value_parser = parse_duration)]
        pub start_timeout_ms: Duration,

        /// The consensus to use for the spawned revive-dev-node.
        #[clap(default_value = "instant-seal")]
        pub consensus: String,

        /// The logging configuration to pass to the binary when it's being started.
        #[clap(default_value = "error,evm=debug,sc_rpc_server=info,runtime::revive=debug")]
        pub logging_level: String,

        /// Specifies the connection string of an existing node that's not managed by the
        /// framework.
        ///
        /// If this argument is specified then the framework will not spawn certain nodes itself
        /// but rather it will opt to using the existing node's through their provided connection
        /// strings.
        pub existing_rpc_url: Vec<String>,
    }

    /// A set of configuration parameters for the polkadot-omni-node.
    #[configuration(key = "polkadot-omni-node")]
    pub struct PolkadotOmnichainNodeConfiguration {
        /// Specifies the path of the polkadot-omni-node to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the
        /// polkadot-omni-node binary that's provided in the user's $PATH.
        #[clap(default_value = "polkadot-omni-node")]
        pub path: PathBuf,

        /// The amount of time to wait upon startup before considering that the node timed out.
        #[clap(default_value = "90000", value_parser = parse_duration)]
        pub start_timeout_ms: Duration,

        /// Defines how often blocks will be sealed by the node in milliseconds.
        #[clap(default_value = "200", value_parser = parse_duration)]
        pub block_time_ms: Duration,

        /// The path of the chainspec of the chain that we're spawning
        pub chain_spec_path: Option<PathBuf>,

        /// The ID of the parachain that the polkadot-omni-node will spawn. This argument is
        /// required if the polkadot-omni-node is one of the selected platforms for running the
        /// tests or benchmarks.
        pub parachain_id: Option<usize>,

        /// The logging configuration to pass to the binary when it's being started.
        #[clap(default_value = "error,evm=debug,sc_rpc_server=info,runtime::revive=debug")]
        pub logging_level: String,
    }

    /// A set of configuration parameters for the ETH RPC.
    #[configuration(key = "eth-rpc")]
    pub struct EthRpcConfiguration {
        /// Specifies the path of the ETH RPC to be used by the tool.
        ///
        /// If this is not specified, then the tool assumes that it should use the ETH RPC binary
        /// that's provided in the user's $PATH.
        #[clap(default_value = "eth-rpc")]
        pub path: PathBuf,

        /// The amount of time to wait upon startup before considering that the node timed out.
        #[clap(default_value = "30000", value_parser = parse_duration)]
        pub start_timeout_ms: Duration,

        /// The logging configuration to pass to the binary when it's being started.
        #[clap(default_value = "info,eth-rpc=debug")]
        pub logging_level: String,
    }

    /// A set of configuration parameters for the genesis.
    #[derive(Default)]
    #[configuration(key = "genesis")]
    pub struct GenesisConfiguration {
        /// Specifies the path of the genesis file to use for the nodes that are started.
        ///
        /// This is expected to be the path of a JSON geth genesis file.
        path: Option<PathBuf>,

        /// The genesis object found at the provided path.
        #[clap(skip)]
        #[serde(skip)]
        genesis: OnceLock<Genesis>,
    }

    impl GenesisConfiguration {
        pub fn genesis(&self) -> anyhow::Result<&Genesis> {
            static DEFAULT_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
                let genesis = include_str!("../../../assets/dev-genesis.json");
                serde_json::from_str(genesis).unwrap()
            });

            match self.genesis.get() {
                Some(genesis) => Ok(genesis),
                None => {
                    let genesis = match self.path.as_ref() {
                        Some(genesis_path) => {
                            let genesis_content = std::fs::read_to_string(genesis_path)?;
                            serde_json::from_str(genesis_content.as_str())?
                        }
                        None => DEFAULT_GENESIS.clone(),
                    };
                    Ok(self.genesis.get_or_init(|| genesis))
                }
            }
        }
    }

    /// A set of configuration parameters for the wallet.
    #[configuration(key = "wallet")]
    pub struct WalletConfiguration {
        /// The private key of the default signer.
        #[clap(
            long = "wallet.default-private-key",
            default_value = "0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d"
        )]
        default_private_key: B256,

        /// This argument controls which private keys the nodes should have access to and be added
        /// to its wallet signers. With a value of N, private keys (0, N] will be added to the
        /// signer set of the node.
        #[clap(default_value_t = 100_000)]
        pub additional_keys: usize,

        /// The wallet object that will be used.
        #[clap(skip)]
        #[serde(skip)]
        wallet: OnceLock<Arc<EthereumWallet>>,
    }

    impl WalletConfiguration {
        pub fn wallet(&self) -> Arc<EthereumWallet> {
            self.wallet
                .get_or_init(|| {
                    let mut wallet = EthereumWallet::new(
                        PrivateKeySigner::from_bytes(&self.default_private_key).unwrap(),
                    );
                    for signer in (1..=self.additional_keys)
                        .map(|id| U256::from(id))
                        .map(|id| id.to_be_bytes::<32>())
                        .map(|id| PrivateKeySigner::from_bytes(&FixedBytes(id)).unwrap())
                    {
                        wallet.register_signer(signer);
                    }
                    Arc::new(wallet)
                })
                .clone()
        }

        pub fn highest_private_key_exclusive(&self) -> U256 {
            U256::try_from(self.additional_keys).unwrap()
        }
    }

    /// A set of configuration for concurrency.
    #[configuration(key = "concurrency")]
    pub struct ConcurrencyConfiguration {
        /// Determines the amount of nodes that will be spawned for each chain.
        #[clap(default_value_t = 5)]
        pub number_of_nodes: usize,

        /// Determines the amount of tokio worker threads that will will be used.
        #[arg(
            default_value_t = std::thread::available_parallelism()
                .map(|n| n.get() * 4 / 6)
                .unwrap_or(1)
        )]
        pub number_of_threads: usize,

        /// Determines the amount of concurrent tasks that will be spawned to run tests. This
        /// means that at any given time there is
        /// `concurrency.number-of-concurrent-tasks` tests concurrently executing.
        ///
        /// Note that a task limit of `0` means no limit on the number of concurrent tasks.
        #[arg(default_value_t = 500)]
        number_of_concurrent_tasks: usize,
    }

    impl ConcurrencyConfiguration {
        pub fn concurrency_limit(&self) -> Option<usize> {
            if self.number_of_concurrent_tasks == 0 {
                None
            } else {
                Some(self.number_of_concurrent_tasks)
            }
        }
    }

    /// A set of configuration parameters for compilation.
    #[configuration(key = "compilation")]
    pub struct CompilationConfiguration {
        /// Controls if the compilation cache should be invalidated or not.
        pub invalidate_cache: bool,
    }

    /// A set of configuration parameters for the report.
    #[configuration(key = "report")]
    pub struct ReportConfiguration {
        /// Controls if the compiler input is included in the final report.
        pub include_compiler_input: bool,

        /// Controls if the compiler output is included in the final report.
        pub include_compiler_output: bool,

        /// The filename to use for the report.
        pub file_name: Option<String>,
    }

    /// A set of configuration parameters for ignoring certain test cases.
    #[configuration(key = "ignore")]
    pub struct IgnoreCasesConfiguration {
        /// The path to a report where all of the succeeding cases found in should be ignored.
        pub succeeding_cases_from_report: Option<PathBuf>,

        /// Allows the tool to ignore any test case where there's a step that's expected to fail.
        pub cases_with_failing_steps: bool,
    }

    impl Default for IgnoreCasesConfiguration {
        fn default() -> Self {
            IgnoreCasesConfiguration::parse_from(["ignore-cases-configuration"])
        }
    }

    impl Context {
        pub fn update_for_profile(&mut self) {
            match self {
                Context::Test(ctx) => ctx.update_for_profile(),
                Context::Benchmark(ctx) => ctx.update_for_profile(),
                Context::ExportJsonSchema(_) => {}
                Context::ExportGenesis(_) => {}
            }
        }
    }

    impl Test {
        pub fn update_for_profile(&mut self) {
            match self.profile.profile {
                Profile::Default => {}
                Profile::Debug => {
                    let default_concurrency_config =
                        ConcurrencyConfiguration::parse_from(["concurrency-configuration"]);
                    let working_directory_config = WorkingDirectoryPath::default();

                    if self.concurrency.number_of_nodes
                        == default_concurrency_config.number_of_nodes
                    {
                        self.concurrency.number_of_nodes = 1;
                    }
                    if self.concurrency.number_of_threads
                        == default_concurrency_config.number_of_threads
                    {
                        self.concurrency.number_of_threads = 5;
                    }
                    if self.concurrency.number_of_concurrent_tasks
                        == default_concurrency_config.number_of_concurrent_tasks
                    {
                        self.concurrency.number_of_concurrent_tasks = 1;
                    }

                    if working_directory_config == self.working_directory.working_directory {
                        let home_directory = std::path::PathBuf::from(
                            std::env::var("HOME").expect("Home dir not found"),
                        );
                        let wd = home_directory.join(".retester-workdir");
                        self.working_directory.working_directory = WorkingDirectoryPath::Path(wd);
                    }
                }
            }
        }
    }

    impl Default for Test {
        fn default() -> Self {
            Self::parse_from(["test", "--test", "."])
        }
    }

    impl Benchmark {
        pub fn update_for_profile(&mut self) {
            match self.profile.profile {
                Profile::Default => {}
                Profile::Debug => {
                    let default_concurrency_config =
                        ConcurrencyConfiguration::parse_from(["concurrency-configuration"]);
                    let working_directory_config = WorkingDirectoryPath::default();

                    if self.concurrency.number_of_nodes
                        == default_concurrency_config.number_of_nodes
                    {
                        self.concurrency.number_of_nodes = 1;
                    }
                    if self.concurrency.number_of_threads
                        == default_concurrency_config.number_of_threads
                    {
                        self.concurrency.number_of_threads = 5;
                    }
                    if self.concurrency.number_of_concurrent_tasks
                        == default_concurrency_config.number_of_concurrent_tasks
                    {
                        self.concurrency.number_of_concurrent_tasks = 1;
                    }

                    if working_directory_config == self.working_directory.working_directory {
                        let home_directory = std::path::PathBuf::from(
                            std::env::var("HOME").expect("Home dir not found"),
                        );
                        let wd = home_directory.join(".retester-workdir");
                        self.working_directory.working_directory = WorkingDirectoryPath::Path(wd);
                    }
                }
            }
        }
    }

    impl Default for Benchmark {
        fn default() -> Self {
            Self::parse_from(["benchmark", "--test", "."])
        }
    }
}

/// Represents the working directory that the program uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingDirectoryPath {
    /// A temporary directory is used as the working directory. This will be removed when dropped.
    TemporaryDirectory(Arc<TempDir>),
    /// A directory with a path is used as the working directory.
    Path(PathBuf),
}

impl Serialize for WorkingDirectoryPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_path().serialize(serializer)
    }
}

impl<'a> Deserialize<'a> for WorkingDirectoryPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        PathBuf::deserialize(deserializer).map(Self::Path)
    }
}

impl WorkingDirectoryPath {
    pub fn as_path(&self) -> &Path {
        self.as_ref()
    }
}

impl Deref for WorkingDirectoryPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.as_path()
    }
}

impl AsRef<Path> for WorkingDirectoryPath {
    fn as_ref(&self) -> &Path {
        match self {
            WorkingDirectoryPath::TemporaryDirectory(temp_dir) => temp_dir.path(),
            WorkingDirectoryPath::Path(path) => path.as_path(),
        }
    }
}

impl Default for WorkingDirectoryPath {
    fn default() -> Self {
        TempDir::new()
            .map(Arc::new)
            .map(Self::TemporaryDirectory)
            .expect("Failed to create the temporary directory")
    }
}

impl FromStr for WorkingDirectoryPath {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" => Ok(Default::default()),
            _ => PathBuf::from(s)
                .canonicalize()
                .context("Failed to canonicalize the working directory path")
                .map(Self::Path),
        }
    }
}

impl Display for WorkingDirectoryPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.as_path().display(), f)
    }
}

/// The output format to use for the test execution output.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum OutputFormat {
    /// The legacy format that was used in the past for the output.
    Legacy,

    /// An output format that looks heavily resembles the output from `cargo test`.
    CargoTestLike,
}

/// Command line profiles used to override the default values provided for the commands.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum Profile {
    /// The default profile used by the framework. This profile is optimized to make the test
    /// and workload execution happen as fast as possible.
    #[default]
    Default,

    /// A debug profile optimized for use cases when certain tests are being debugged. This profile
    /// sets up the framework with the following:
    ///
    /// * `concurrency.number-of-nodes` set to 1 node.
    /// * `concurrency.number-of-concurrent-tasks` set to 1 such that tests execute sequentially.
    /// * `concurrency.number-of-threads` set to 5.
    /// * `working-directory` set to ~/.retester-workdir
    Debug,
}

fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    u64::from_str(s)
        .map(Duration::from_millis)
        .map_err(Into::into)
}
