//! The global configuration used across all revive differential testing crates.

use std::{
    fmt::Display,
    fs::read_to_string,
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
use clap::{Parser, ValueEnum, ValueHint};
use revive_dt_common::types::{ParsedTestSpecifier, PlatformIdentifier};
use semver::Version;
use serde::{Deserialize, Serialize, Serializer};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use temp_dir::TempDir;

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
#[command(name = "retester")]
pub enum Context {
    /// Executes tests in the MatterLabs format differentially on multiple targets concurrently.
    Test(Box<TestExecutionContext>),

    /// Executes differential benchmarks on various platforms.
    Benchmark(Box<BenchmarkingContext>),

    /// Exports the JSON schema of the MatterLabs test format used by the tool.
    ExportJsonSchema,

    /// Exports the genesis file of the desired platform.
    ExportGenesis(Box<ExportGenesisContext>),
}

impl Context {
    pub fn working_directory_configuration(&self) -> &WorkingDirectoryConfiguration {
        self.as_ref()
    }

    pub fn report_configuration(&self) -> &ReportConfiguration {
        self.as_ref()
    }

    pub fn update_for_profile(&mut self) {
        match self {
            Context::Test(ctx) => ctx.update_for_profile(),
            Context::Benchmark(ctx) => ctx.update_for_profile(),
            Context::ExportJsonSchema => {}
            Context::ExportGenesis(..) => {}
        }
    }
}

impl AsRef<WorkingDirectoryConfiguration> for Context {
    fn as_ref(&self) -> &WorkingDirectoryConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<CorpusConfiguration> for Context {
    fn as_ref(&self) -> &CorpusConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<SolcConfiguration> for Context {
    fn as_ref(&self) -> &SolcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<ResolcConfiguration> for Context {
    fn as_ref(&self) -> &ResolcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<GethConfiguration> for Context {
    fn as_ref(&self) -> &GethConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<KurtosisConfiguration> for Context {
    fn as_ref(&self) -> &KurtosisConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<PolkadotParachainConfiguration> for Context {
    fn as_ref(&self) -> &PolkadotParachainConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<KitchensinkConfiguration> for Context {
    fn as_ref(&self) -> &KitchensinkConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ReviveDevNodeConfiguration> for Context {
    fn as_ref(&self) -> &ReviveDevNodeConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<EthRpcConfiguration> for Context {
    fn as_ref(&self) -> &EthRpcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<GenesisConfiguration> for Context {
    fn as_ref(&self) -> &GenesisConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(..) | Self::ExportGenesis(..) => {
                static GENESIS: LazyLock<GenesisConfiguration> = LazyLock::new(Default::default);
                &GENESIS
            }
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<WalletConfiguration> for Context {
    fn as_ref(&self) -> &WalletConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportGenesis(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ConcurrencyConfiguration> for Context {
    fn as_ref(&self) -> &ConcurrencyConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<CompilationConfiguration> for Context {
    fn as_ref(&self) -> &CompilationConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<ReportConfiguration> for Context {
    fn as_ref(&self) -> &ReportConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

impl AsRef<IgnoreSuccessConfiguration> for Context {
    fn as_ref(&self) -> &IgnoreSuccessConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(..) => unreachable!(),
            Self::ExportJsonSchema | Self::ExportGenesis(..) => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct TestExecutionContext {
    /// The commandline profile to use. Different profiles change the defaults of the various cli
    /// arguments.
    #[arg(long = "profile", default_value_t = Profile::Default)]
    pub profile: Profile,

    /// The set of platforms that the differential tests should run on.
    #[arg(
        short = 'p',
        long = "platform",
        default_values = ["geth-evm-solc", "revive-dev-node-polkavm-resolc"]
    )]
    pub platforms: Vec<PlatformIdentifier>,

    /// The output format to use for the tool's output.
    #[arg(short, long, default_value_t = OutputFormat::CargoTestLike)]
    pub output_format: OutputFormat,

    /// The working directory that the program will use for all of the temporary artifacts needed at
    /// runtime.
    ///
    /// If not specified, then a temporary directory will be created and used by the program for all
    /// temporary artifacts.
    #[clap(
        short,
        long,
        default_value = "",
        value_hint = ValueHint::DirPath,
    )]
    pub working_directory: WorkingDirectoryConfiguration,

    /// Configuration parameters for the corpus files to use.
    #[clap(flatten, next_help_heading = "Corpus Configuration")]
    pub corpus_configuration: CorpusConfiguration,

    /// Configuration parameters for the solc compiler.
    #[clap(flatten, next_help_heading = "Solc Configuration")]
    pub solc_configuration: SolcConfiguration,

    /// Configuration parameters for the resolc compiler.
    #[clap(flatten, next_help_heading = "Resolc Configuration")]
    pub resolc_configuration: ResolcConfiguration,

    /// Configuration parameters for the Polkadot Parachain.
    #[clap(flatten, next_help_heading = "Polkadot Parachain Configuration")]
    pub polkadot_parachain_configuration: PolkadotParachainConfiguration,

    /// Configuration parameters for the geth node.
    #[clap(flatten, next_help_heading = "Geth Configuration")]
    pub geth_configuration: GethConfiguration,

    /// Configuration parameters for the lighthouse node.
    #[clap(flatten, next_help_heading = "Lighthouse Configuration")]
    pub lighthouse_configuration: KurtosisConfiguration,

    /// Configuration parameters for the Kitchensink.
    #[clap(flatten, next_help_heading = "Kitchensink Configuration")]
    pub kitchensink_configuration: KitchensinkConfiguration,

    /// Configuration parameters for the Revive Dev Node.
    #[clap(flatten, next_help_heading = "Revive Dev Node Configuration")]
    pub revive_dev_node_configuration: ReviveDevNodeConfiguration,

    /// Configuration parameters for the Eth Rpc.
    #[clap(flatten, next_help_heading = "Eth RPC Configuration")]
    pub eth_rpc_configuration: EthRpcConfiguration,

    /// Configuration parameters for the genesis.
    #[clap(flatten, next_help_heading = "Genesis Configuration")]
    pub genesis_configuration: GenesisConfiguration,

    /// Configuration parameters for the wallet.
    #[clap(flatten, next_help_heading = "Wallet Configuration")]
    pub wallet_configuration: WalletConfiguration,

    /// Configuration parameters for concurrency.
    #[clap(flatten, next_help_heading = "Concurrency Configuration")]
    pub concurrency_configuration: ConcurrencyConfiguration,

    /// Configuration parameters for the compilers and compilation.
    #[clap(flatten, next_help_heading = "Compilation Configuration")]
    pub compilation_configuration: CompilationConfiguration,

    /// Configuration parameters for the report.
    #[clap(flatten, next_help_heading = "Report Configuration")]
    pub report_configuration: ReportConfiguration,

    /// Configuration parameters for ignoring certain test cases based on the report
    #[clap(flatten, next_help_heading = "Ignore Success Configuration")]
    pub ignore_success_configuration: IgnoreSuccessConfiguration,
}

impl TestExecutionContext {
    pub fn update_for_profile(&mut self) {
        match self.profile {
            Profile::Default => {}
            Profile::Debug => {
                let default_concurrency_config =
                    ConcurrencyConfiguration::parse_from(["concurrency-configuration"]);
                let working_directory_config = WorkingDirectoryConfiguration::default();

                if self.concurrency_configuration.number_of_nodes
                    == default_concurrency_config.number_of_nodes
                {
                    self.concurrency_configuration.number_of_nodes = 1;
                }
                if self.concurrency_configuration.number_of_threads
                    == default_concurrency_config.number_of_threads
                {
                    self.concurrency_configuration.number_of_threads = 5;
                }
                if self.concurrency_configuration.number_concurrent_tasks
                    == default_concurrency_config.number_concurrent_tasks
                {
                    self.concurrency_configuration.number_concurrent_tasks = 1;
                }

                if working_directory_config == self.working_directory {
                    let home_directory =
                        PathBuf::from(std::env::var("HOME").expect("Home dir not found"));
                    let working_directory = home_directory.join(".retester-workdir");
                    self.working_directory = WorkingDirectoryConfiguration::Path(working_directory)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct BenchmarkingContext {
    /// The commandline profile to use. Different profiles change the defaults of the various cli
    /// arguments.
    #[arg(long = "profile", default_value_t = Profile::Default)]
    pub profile: Profile,

    /// The working directory that the program will use for all of the temporary artifacts needed at
    /// runtime.
    ///
    /// If not specified, then a temporary directory will be created and used by the program for all
    /// temporary artifacts.
    #[clap(
        short,
        long,
        default_value = "",
        value_hint = ValueHint::DirPath,
    )]
    pub working_directory: WorkingDirectoryConfiguration,

    /// The set of platforms that the differential tests should run on.
    #[arg(
        short = 'p',
        long = "platform",
        default_values = ["geth-evm-solc", "revive-dev-node-polkavm-resolc"]
    )]
    pub platforms: Vec<PlatformIdentifier>,

    /// The default repetition count for any workload specified but that doesn't contain a repeat
    /// step.
    #[arg(short = 'r', long = "default-repetition-count", default_value_t = 1000)]
    pub default_repetition_count: usize,

    /// Configuration parameters for the corpus files to use.
    #[clap(flatten, next_help_heading = "Corpus Configuration")]
    pub corpus_configuration: CorpusConfiguration,

    /// Configuration parameters for the solc compiler.
    #[clap(flatten, next_help_heading = "Solc Configuration")]
    pub solc_configuration: SolcConfiguration,

    /// Configuration parameters for the resolc compiler.
    #[clap(flatten, next_help_heading = "Resolc Configuration")]
    pub resolc_configuration: ResolcConfiguration,

    /// Configuration parameters for the geth node.
    #[clap(flatten, next_help_heading = "Geth Configuration")]
    pub geth_configuration: GethConfiguration,

    /// Configuration parameters for the lighthouse node.
    #[clap(flatten, next_help_heading = "Lighthouse Configuration")]
    pub lighthouse_configuration: KurtosisConfiguration,

    /// Configuration parameters for the Kitchensink.
    #[clap(flatten, next_help_heading = "Kitchensink Configuration")]
    pub kitchensink_configuration: KitchensinkConfiguration,

    /// Configuration parameters for the Polkadot Parachain.
    #[clap(flatten, next_help_heading = "Polkadot Parachain Configuration")]
    pub polkadot_parachain_configuration: PolkadotParachainConfiguration,

    /// Configuration parameters for the Revive Dev Node.
    #[clap(flatten, next_help_heading = "Revive Dev Node Configuration")]
    pub revive_dev_node_configuration: ReviveDevNodeConfiguration,

    /// Configuration parameters for the Eth Rpc.
    #[clap(flatten, next_help_heading = "Eth RPC Configuration")]
    pub eth_rpc_configuration: EthRpcConfiguration,

    /// Configuration parameters for the wallet.
    #[clap(flatten, next_help_heading = "Wallet Configuration")]
    pub wallet_configuration: WalletConfiguration,

    /// Configuration parameters for concurrency.
    #[clap(flatten, next_help_heading = "Concurrency Configuration")]
    pub concurrency_configuration: ConcurrencyConfiguration,

    /// Configuration parameters for the compilers and compilation.
    #[clap(flatten, next_help_heading = "Compilation Configuration")]
    pub compilation_configuration: CompilationConfiguration,

    /// Configuration parameters for the report.
    #[clap(flatten, next_help_heading = "Report Configuration")]
    pub report_configuration: ReportConfiguration,
}

impl BenchmarkingContext {
    pub fn update_for_profile(&mut self) {
        match self.profile {
            Profile::Default => {}
            Profile::Debug => {
                let default_concurrency_config =
                    ConcurrencyConfiguration::parse_from(["concurrency-configuration"]);
                let working_directory_config = WorkingDirectoryConfiguration::default();

                if self.concurrency_configuration.number_of_nodes
                    == default_concurrency_config.number_of_nodes
                {
                    self.concurrency_configuration.number_of_nodes = 1;
                }
                if self.concurrency_configuration.number_of_threads
                    == default_concurrency_config.number_of_threads
                {
                    self.concurrency_configuration.number_of_threads = 5;
                }
                if self.concurrency_configuration.number_concurrent_tasks
                    == default_concurrency_config.number_concurrent_tasks
                {
                    self.concurrency_configuration.number_concurrent_tasks = 1;
                }

                if working_directory_config == self.working_directory {
                    let home_directory =
                        PathBuf::from(std::env::var("HOME").expect("Home dir not found"));
                    let working_directory = home_directory.join(".retester-workdir");
                    self.working_directory = WorkingDirectoryConfiguration::Path(working_directory)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct ExportGenesisContext {
    /// The platform of choice to export the genesis for.
    pub platform: PlatformIdentifier,

    /// Configuration parameters for the geth node.
    #[clap(flatten, next_help_heading = "Geth Configuration")]
    pub geth_configuration: GethConfiguration,

    /// Configuration parameters for the lighthouse node.
    #[clap(flatten, next_help_heading = "Lighthouse Configuration")]
    pub lighthouse_configuration: KurtosisConfiguration,

    /// Configuration parameters for the Kitchensink.
    #[clap(flatten, next_help_heading = "Kitchensink Configuration")]
    pub kitchensink_configuration: KitchensinkConfiguration,

    /// Configuration parameters for the Polkadot Parachain.
    #[clap(flatten, next_help_heading = "Polkadot Parachain Configuration")]
    pub polkadot_parachain_configuration: PolkadotParachainConfiguration,

    /// Configuration parameters for the Revive Dev Node.
    #[clap(flatten, next_help_heading = "Revive Dev Node Configuration")]
    pub revive_dev_node_configuration: ReviveDevNodeConfiguration,

    /// Configuration parameters for the wallet.
    #[clap(flatten, next_help_heading = "Wallet Configuration")]
    pub wallet_configuration: WalletConfiguration,
}

impl Default for TestExecutionContext {
    fn default() -> Self {
        Self::parse_from(["execution-context"])
    }
}

impl AsRef<WorkingDirectoryConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &WorkingDirectoryConfiguration {
        &self.working_directory
    }
}

impl AsRef<CorpusConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &CorpusConfiguration {
        &self.corpus_configuration
    }
}

impl AsRef<SolcConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &SolcConfiguration {
        &self.solc_configuration
    }
}

impl AsRef<ResolcConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &ResolcConfiguration {
        &self.resolc_configuration
    }
}

impl AsRef<GethConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &GethConfiguration {
        &self.geth_configuration
    }
}

impl AsRef<PolkadotParachainConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &PolkadotParachainConfiguration {
        &self.polkadot_parachain_configuration
    }
}

impl AsRef<KurtosisConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &KurtosisConfiguration {
        &self.lighthouse_configuration
    }
}

impl AsRef<KitchensinkConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &KitchensinkConfiguration {
        &self.kitchensink_configuration
    }
}

impl AsRef<ReviveDevNodeConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &ReviveDevNodeConfiguration {
        &self.revive_dev_node_configuration
    }
}

impl AsRef<EthRpcConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &EthRpcConfiguration {
        &self.eth_rpc_configuration
    }
}

impl AsRef<GenesisConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &GenesisConfiguration {
        &self.genesis_configuration
    }
}

impl AsRef<WalletConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &WalletConfiguration {
        &self.wallet_configuration
    }
}

impl AsRef<ConcurrencyConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &ConcurrencyConfiguration {
        &self.concurrency_configuration
    }
}

impl AsRef<CompilationConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &CompilationConfiguration {
        &self.compilation_configuration
    }
}

impl AsRef<ReportConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &ReportConfiguration {
        &self.report_configuration
    }
}

impl AsRef<IgnoreSuccessConfiguration> for TestExecutionContext {
    fn as_ref(&self) -> &IgnoreSuccessConfiguration {
        &self.ignore_success_configuration
    }
}

impl Default for BenchmarkingContext {
    fn default() -> Self {
        Self::parse_from(["benchmarking-context"])
    }
}

impl AsRef<WorkingDirectoryConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &WorkingDirectoryConfiguration {
        &self.working_directory
    }
}

impl AsRef<CorpusConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &CorpusConfiguration {
        &self.corpus_configuration
    }
}

impl AsRef<SolcConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &SolcConfiguration {
        &self.solc_configuration
    }
}

impl AsRef<ResolcConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &ResolcConfiguration {
        &self.resolc_configuration
    }
}

impl AsRef<GethConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &GethConfiguration {
        &self.geth_configuration
    }
}

impl AsRef<KurtosisConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &KurtosisConfiguration {
        &self.lighthouse_configuration
    }
}

impl AsRef<PolkadotParachainConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &PolkadotParachainConfiguration {
        &self.polkadot_parachain_configuration
    }
}

impl AsRef<KitchensinkConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &KitchensinkConfiguration {
        &self.kitchensink_configuration
    }
}

impl AsRef<ReviveDevNodeConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &ReviveDevNodeConfiguration {
        &self.revive_dev_node_configuration
    }
}

impl AsRef<EthRpcConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &EthRpcConfiguration {
        &self.eth_rpc_configuration
    }
}

impl AsRef<WalletConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &WalletConfiguration {
        &self.wallet_configuration
    }
}

impl AsRef<ConcurrencyConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &ConcurrencyConfiguration {
        &self.concurrency_configuration
    }
}

impl AsRef<CompilationConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &CompilationConfiguration {
        &self.compilation_configuration
    }
}

impl AsRef<ReportConfiguration> for BenchmarkingContext {
    fn as_ref(&self) -> &ReportConfiguration {
        &self.report_configuration
    }
}

impl Default for ExportGenesisContext {
    fn default() -> Self {
        Self::parse_from(["export-genesis-context"])
    }
}

impl AsRef<GethConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &GethConfiguration {
        &self.geth_configuration
    }
}

impl AsRef<KurtosisConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &KurtosisConfiguration {
        &self.lighthouse_configuration
    }
}

impl AsRef<KitchensinkConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &KitchensinkConfiguration {
        &self.kitchensink_configuration
    }
}

impl AsRef<PolkadotParachainConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &PolkadotParachainConfiguration {
        &self.polkadot_parachain_configuration
    }
}

impl AsRef<ReviveDevNodeConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &ReviveDevNodeConfiguration {
        &self.revive_dev_node_configuration
    }
}

impl AsRef<WalletConfiguration> for ExportGenesisContext {
    fn as_ref(&self) -> &WalletConfiguration {
        &self.wallet_configuration
    }
}

/// A set of configuration parameters for the corpus files to use for the execution.
#[serde_with::serde_as]
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct CorpusConfiguration {
    /// A list of test specifiers for the tests that the tool should run.
    ///
    /// Test specifiers follow the following format:
    ///
    /// - `{directory_path|metadata_file_path}`: A path to a metadata file where all of the cases
    ///   live and should be run. Alternatively, it points to a directory instructing the framework
    ///   to discover of the metadata files that live there an execute them.
    /// - `{metadata_file_path}::{case_idx}`: The path to a metadata file and then a case idx
    ///   separated by two colons. This specifies that only this specific test case within the
    ///   metadata file should be executed.
    /// - `{metadata_file_path}::{case_idx}::{mode}`: This is very similar to the above specifier
    ///   with the exception that in this case the mode is specified and will be used in the test.
    #[serde_as(as = "Vec<serde_with::DisplayFromStr>")]
    #[arg(short = 't', long = "test")]
    pub test_specifiers: Vec<ParsedTestSpecifier>,
}

/// A set of configuration parameters for Solc.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct SolcConfiguration {
    /// Specifies the default version of the Solc compiler that should be used if there is no
    /// override specified by one of the test cases.
    #[clap(long = "solc.version", default_value = "0.8.29")]
    pub version: Version,
}

/// A set of configuration parameters for Resolc.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct ResolcConfiguration {
    /// Specifies the path of the resolc compiler to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the resolc binary that's
    /// provided in the user's $PATH.
    #[clap(id = "resolc.path", long = "resolc.path", default_value = "resolc")]
    pub path: PathBuf,
}

/// A set of configuration parameters for Polkadot Parachain.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct PolkadotParachainConfiguration {
    /// Specifies the path of the polkadot-parachain node to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the polkadot-parachain binary
    /// that's provided in the user's $PATH.
    #[clap(
        id = "polkadot-parachain.path",
        long = "polkadot-parachain.path",
        default_value = "polkadot-parachain"
    )]
    pub path: PathBuf,

    /// The amount of time to wait upon startup before considering that the node timed out.
    #[clap(
        id = "polkadot-parachain.start-timeout-ms",
        long = "polkadot-parachain.start-timeout-ms",
        default_value = "5000",
        value_parser = parse_duration
    )]
    pub start_timeout_ms: Duration,
}

/// A set of configuration parameters for Geth.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct GethConfiguration {
    /// Specifies the path of the geth node to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the geth binary that's
    /// provided in the user's $PATH.
    #[clap(id = "geth.path", long = "geth.path", default_value = "geth")]
    pub path: PathBuf,

    /// The amount of time to wait upon startup before considering that the node timed out.
    #[clap(
        id = "geth.start-timeout-ms",
        long = "geth.start-timeout-ms",
        default_value = "30000",
        value_parser = parse_duration
    )]
    pub start_timeout_ms: Duration,
}

/// A set of configuration parameters for kurtosis.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct KurtosisConfiguration {
    /// Specifies the path of the kurtosis node to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the kurtosis binary that's
    /// provided in the user's $PATH.
    #[clap(
        id = "kurtosis.path",
        long = "kurtosis.path",
        default_value = "kurtosis"
    )]
    pub path: PathBuf,
}

/// A set of configuration parameters for Kitchensink.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct KitchensinkConfiguration {
    /// Specifies the path of the kitchensink node to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the kitchensink binary
    /// that's provided in the user's $PATH.
    #[clap(
        id = "kitchensink.path",
        long = "kitchensink.path",
        default_value = "substrate-node"
    )]
    pub path: PathBuf,

    /// The amount of time to wait upon startup before considering that the node timed out.
    #[clap(
        id = "kitchensink.start-timeout-ms",
        long = "kitchensink.start-timeout-ms",
        default_value = "30000",
        value_parser = parse_duration
    )]
    pub start_timeout_ms: Duration,
}

/// A set of configuration parameters for the revive dev node.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct ReviveDevNodeConfiguration {
    /// Specifies the path of the revive dev node to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the revive dev node binary
    /// that's provided in the user's $PATH.
    #[clap(
        id = "revive-dev-node.path",
        long = "revive-dev-node.path",
        default_value = "revive-dev-node"
    )]
    pub path: PathBuf,

    /// The amount of time to wait upon startup before considering that the node timed out.
    #[clap(
        id = "revive-dev-node.start-timeout-ms",
        long = "revive-dev-node.start-timeout-ms",
        default_value = "30000",
        value_parser = parse_duration
    )]
    pub start_timeout_ms: Duration,

    /// The consensus to use for the spawned revive-dev-node.
    #[clap(
        id = "revive-dev-node.consensus",
        long = "revive-dev-node.consensus",
        default_value = "instant-seal"
    )]
    pub consensus: String,

    /// Specifies the connection string of an existing node that's not managed by the framework.
    ///
    /// If this argument is specified then the framework will not spawn certain nodes itself but
    /// rather it will opt to using the existing node's through their provided connection strings.
    ///
    /// This means that if `ConcurrencyConfiguration.number_of_nodes` is 10 and we only specify the
    /// connection strings of 2 nodes here, then nodes 0 and 1 will use the provided connection
    /// strings and nodes 2 through 10 (exclusive) will all be spawned and managed by the framework.
    ///
    /// Thus, if you want all of the transactions and tests to happen against the node that you
    /// spawned and manage then you need to specify a `ConcurrencyConfiguration.number_of_nodes` of
    /// 1.
    #[clap(
        id = "revive-dev-node.existing-rpc-url",
        long = "revive-dev-node.existing-rpc-url"
    )]
    pub existing_rpc_url: Vec<String>,
}

/// A set of configuration parameters for the ETH RPC.
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct EthRpcConfiguration {
    /// Specifies the path of the ETH RPC to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the ETH RPC binary
    /// that's provided in the user's $PATH.
    #[clap(id = "eth-rpc.path", long = "eth-rpc.path", default_value = "eth-rpc")]
    pub path: PathBuf,

    /// The amount of time to wait upon startup before considering that the node timed out.
    #[clap(
        id = "eth-rpc.start-timeout-ms",
        long = "eth-rpc.start-timeout-ms",
        default_value = "30000",
        value_parser = parse_duration
    )]
    pub start_timeout_ms: Duration,
}

/// A set of configuration parameters for the genesis.
#[derive(Clone, Debug, Default, Parser, Serialize, Deserialize)]
pub struct GenesisConfiguration {
    /// Specifies the path of the genesis file to use for the nodes that are started.
    ///
    /// This is expected to be the path of a JSON geth genesis file.
    #[clap(id = "genesis.path", long = "genesis.path")]
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
                        let genesis_content = read_to_string(genesis_path)?;
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
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct WalletConfiguration {
    /// The private key of the default signer.
    #[clap(
        long = "wallet.default-private-key",
        default_value = "0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d"
    )]
    default_key: B256,

    /// This argument controls which private keys the nodes should have access to and be added to
    /// its wallet signers. With a value of N, private keys (0, N] will be added to the signer set
    /// of the node.
    #[clap(long = "wallet.additional-keys", default_value_t = 100_000)]
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
                let mut wallet =
                    EthereumWallet::new(PrivateKeySigner::from_bytes(&self.default_key).unwrap());
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
#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct ConcurrencyConfiguration {
    /// Determines the amount of nodes that will be spawned for each chain.
    #[clap(long = "concurrency.number-of-nodes", default_value_t = 5)]
    pub number_of_nodes: usize,

    /// Determines the amount of tokio worker threads that will will be used.
    #[arg(
        long = "concurrency.number-of-threads",
        default_value_t = std::thread::available_parallelism()
            .map(|n| n.get() * 4 / 6)
            .unwrap_or(1)
    )]
    pub number_of_threads: usize,

    /// Determines the amount of concurrent tasks that will be spawned to run tests. This means that
    /// at any given time there is `concurrency.number-of-concurrent-tasks` tests concurrently
    /// executing.
    ///
    /// Note that a task limit of `0` means no limit on the number of concurrent tasks.
    #[arg(long = "concurrency.number-of-concurrent-tasks", default_value_t = 500)]
    number_concurrent_tasks: usize,
}

impl ConcurrencyConfiguration {
    pub fn concurrency_limit(&self) -> Option<usize> {
        if self.number_concurrent_tasks == 0 {
            None
        } else {
            Some(self.number_concurrent_tasks)
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct CompilationConfiguration {
    /// Controls if the compilation cache should be invalidated or not.
    #[arg(long = "compilation.invalidate-cache")]
    pub invalidate_compilation_cache: bool,
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct ReportConfiguration {
    /// Controls if the compiler input is included in the final report.
    #[clap(long = "report.include-compiler-input")]
    pub include_compiler_input: bool,

    /// Controls if the compiler output is included in the final report.
    #[clap(long = "report.include-compiler-output")]
    pub include_compiler_output: bool,
}

#[derive(Clone, Debug, Parser, Serialize, Deserialize)]
pub struct IgnoreSuccessConfiguration {
    /// The path of the report generated by the tool to use to ignore the cases that succeeded.
    #[clap(long = "ignore-success.report-path")]
    pub path: Option<PathBuf>,
}

/// Represents the working directory that the program uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingDirectoryConfiguration {
    /// A temporary directory is used as the working directory. This will be removed when dropped.
    TemporaryDirectory(Arc<TempDir>),
    /// A directory with a path is used as the working directory.
    Path(PathBuf),
}

impl Serialize for WorkingDirectoryConfiguration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_path().serialize(serializer)
    }
}

impl<'a> Deserialize<'a> for WorkingDirectoryConfiguration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        PathBuf::deserialize(deserializer).map(Self::Path)
    }
}

impl WorkingDirectoryConfiguration {
    pub fn as_path(&self) -> &Path {
        self.as_ref()
    }
}

impl Deref for WorkingDirectoryConfiguration {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.as_path()
    }
}

impl AsRef<Path> for WorkingDirectoryConfiguration {
    fn as_ref(&self) -> &Path {
        match self {
            WorkingDirectoryConfiguration::TemporaryDirectory(temp_dir) => temp_dir.path(),
            WorkingDirectoryConfiguration::Path(path) => path.as_path(),
        }
    }
}

impl Default for WorkingDirectoryConfiguration {
    fn default() -> Self {
        TempDir::new()
            .map(Arc::new)
            .map(Self::TemporaryDirectory)
            .expect("Failed to create the temporary directory")
    }
}

impl FromStr for WorkingDirectoryConfiguration {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" => Ok(Default::default()),
            _ => Ok(Self::Path(PathBuf::from(s))),
        }
    }
}

impl Display for WorkingDirectoryConfiguration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.as_path().display(), f)
    }
}

fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    u64::from_str(s)
        .map(Duration::from_millis)
        .map_err(Into::into)
}

/// The Solidity compatible node implementation.
///
/// This describes the solutions to be tested against on a high level.
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
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum TestingPlatform {
    /// The go-ethereum reference full node EVM implementation.
    Geth,
    /// The kitchensink runtime provides the PolkaVM (PVM) based node implementation.
    Kitchensink,
    /// A polkadot/Substrate based network
    Zombienet,
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
