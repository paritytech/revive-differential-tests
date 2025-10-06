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
    hex::ToHexExt,
    network::EthereumWallet,
    primitives::{FixedBytes, U256},
    signers::local::PrivateKeySigner,
};
use clap::{Parser, ValueEnum, ValueHint};
use revive_dt_common::types::PlatformIdentifier;
use semver::Version;
use serde::{Serialize, Serializer};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use temp_dir::TempDir;

#[derive(Clone, Debug, Parser, Serialize)]
#[command(name = "retester")]
pub enum Context {
    /// Executes tests in the MatterLabs format differentially on multiple targets concurrently.
    Test(Box<TestExecutionContext>),

    /// Executes differential benchmarks on various platforms.
    Benchmark(Box<BenchmarkingContext>),

    /// Exports the JSON schema of the MatterLabs test format used by the tool.
    ExportJsonSchema,
}

impl Context {
    pub fn working_directory_configuration(&self) -> &WorkingDirectoryConfiguration {
        self.as_ref()
    }

    pub fn report_configuration(&self) -> &ReportConfiguration {
        self.as_ref()
    }
}

impl AsRef<WorkingDirectoryConfiguration> for Context {
    fn as_ref(&self) -> &WorkingDirectoryConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<CorpusConfiguration> for Context {
    fn as_ref(&self) -> &CorpusConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<SolcConfiguration> for Context {
    fn as_ref(&self) -> &SolcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ResolcConfiguration> for Context {
    fn as_ref(&self) -> &ResolcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<GethConfiguration> for Context {
    fn as_ref(&self) -> &GethConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<KurtosisConfiguration> for Context {
    fn as_ref(&self) -> &KurtosisConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<PolkadotParachainConfiguration> for Context {
    fn as_ref(&self) -> &PolkadotParachainConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<KitchensinkConfiguration> for Context {
    fn as_ref(&self) -> &KitchensinkConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ReviveDevNodeConfiguration> for Context {
    fn as_ref(&self) -> &ReviveDevNodeConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<EthRpcConfiguration> for Context {
    fn as_ref(&self) -> &EthRpcConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<GenesisConfiguration> for Context {
    fn as_ref(&self) -> &GenesisConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(..) => {
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
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ConcurrencyConfiguration> for Context {
    fn as_ref(&self) -> &ConcurrencyConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<CompilationConfiguration> for Context {
    fn as_ref(&self) -> &CompilationConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

impl AsRef<ReportConfiguration> for Context {
    fn as_ref(&self) -> &ReportConfiguration {
        match self {
            Self::Test(context) => context.as_ref().as_ref(),
            Self::Benchmark(context) => context.as_ref().as_ref(),
            Self::ExportJsonSchema => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize)]
pub struct TestExecutionContext {
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
}

#[derive(Clone, Debug, Parser, Serialize)]
pub struct BenchmarkingContext {
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

impl Default for BenchmarkingContext {
    fn default() -> Self {
        Self::parse_from(["execution-context"])
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

/// A set of configuration parameters for the corpus files to use for the execution.
#[derive(Clone, Debug, Parser, Serialize)]
pub struct CorpusConfiguration {
    /// A list of test corpus JSON files to be tested.
    #[arg(short = 'c', long = "corpus")]
    pub paths: Vec<PathBuf>,
}

/// A set of configuration parameters for Solc.
#[derive(Clone, Debug, Parser, Serialize)]
pub struct SolcConfiguration {
    /// Specifies the default version of the Solc compiler that should be used if there is no
    /// override specified by one of the test cases.
    #[clap(long = "solc.version", default_value = "0.8.29")]
    pub version: Version,
}

/// A set of configuration parameters for Resolc.
#[derive(Clone, Debug, Parser, Serialize)]
pub struct ResolcConfiguration {
    /// Specifies the path of the resolc compiler to be used by the tool.
    ///
    /// If this is not specified, then the tool assumes that it should use the resolc binary that's
    /// provided in the user's $PATH.
    #[clap(id = "resolc.path", long = "resolc.path", default_value = "resolc")]
    pub path: PathBuf,
}

/// A set of configuration parameters for Polkadot Parachain.
#[derive(Clone, Debug, Parser, Serialize)]
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
#[derive(Clone, Debug, Parser, Serialize)]
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
#[derive(Clone, Debug, Parser, Serialize)]
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
#[derive(Clone, Debug, Parser, Serialize)]
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
#[derive(Clone, Debug, Parser, Serialize)]
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
}

/// A set of configuration parameters for the ETH RPC.
#[derive(Clone, Debug, Parser, Serialize)]
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
#[derive(Clone, Debug, Default, Parser, Serialize)]
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
#[derive(Clone, Debug, Parser, Serialize)]
pub struct WalletConfiguration {
    /// The private key of the default signer.
    #[clap(
        long = "wallet.default-private-key",
        default_value = "0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d"
    )]
    #[serde(serialize_with = "serialize_private_key")]
    default_key: PrivateKeySigner,

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
                let mut wallet = EthereumWallet::new(self.default_key.clone());
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

fn serialize_private_key<S>(value: &PrivateKeySigner, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.to_bytes().encode_hex().serialize(serializer)
}

/// A set of configuration for concurrency.
#[derive(Clone, Debug, Parser, Serialize)]
pub struct ConcurrencyConfiguration {
    /// Determines the amount of nodes that will be spawned for each chain.
    #[clap(long = "concurrency.number-of-nodes", default_value_t = 5)]
    pub number_of_nodes: usize,

    /// Determines the amount of tokio worker threads that will will be used.
    #[arg(
        long = "concurrency.number-of-threads",
        default_value_t = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    )]
    pub number_of_threads: usize,

    /// Determines the amount of concurrent tasks that will be spawned to run tests.
    ///
    /// Defaults to 10 x the number of nodes.
    #[arg(long = "concurrency.number-of-concurrent-tasks")]
    number_concurrent_tasks: Option<usize>,

    /// Determines if the concurrency limit should be ignored or not.
    #[arg(long = "concurrency.ignore-concurrency-limit")]
    ignore_concurrency_limit: bool,
}

impl ConcurrencyConfiguration {
    pub fn concurrency_limit(&self) -> Option<usize> {
        match self.ignore_concurrency_limit {
            true => None,
            false => Some(
                self.number_concurrent_tasks
                    .unwrap_or(20 * self.number_of_nodes),
            ),
        }
    }
}

#[derive(Clone, Debug, Parser, Serialize)]
pub struct CompilationConfiguration {
    /// Controls if the compilation cache should be invalidated or not.
    #[arg(long = "compilation.invalidate-cache")]
    pub invalidate_compilation_cache: bool,
}

#[derive(Clone, Debug, Parser, Serialize)]
pub struct ReportConfiguration {
    /// Controls if the compiler input is included in the final report.
    #[clap(long = "report.include-compiler-input")]
    pub include_compiler_input: bool,

    /// Controls if the compiler output is included in the final report.
    #[clap(long = "report.include-compiler-output")]
    pub include_compiler_output: bool,
}

/// Represents the working directory that the program uses.
#[derive(Debug, Clone)]
pub enum WorkingDirectoryConfiguration {
    /// A temporary directory is used as the working directory. This will be removed when dropped.
    TemporaryDirectory(Arc<TempDir>),
    /// A directory with a path is used as the working directory.
    Path(PathBuf),
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

impl Serialize for WorkingDirectoryConfiguration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_path().serialize(serializer)
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
