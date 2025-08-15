//! The global configuration used across all revive differential testing crates.

use std::{
    fmt::Display,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};
use clap::{Parser, ValueEnum};
use semver::Version;
use serde::{Deserialize, Serialize};
use temp_dir::TempDir;

#[derive(Debug, Parser, Clone, Serialize, Deserialize)]
#[command(name = "retester")]
pub struct Arguments {
    /// The `solc` version to use if the test didn't specify it explicitly.
    #[arg(long = "solc", short, default_value = "0.8.29")]
    pub solc: Version,

    /// Use the Wasm compiler versions.
    #[arg(long = "wasm")]
    pub wasm: bool,

    /// The path to the `resolc` executable to be tested.
    ///
    /// By default it uses the `resolc` binary found in `$PATH`.
    ///
    /// If `--wasm` is set, this should point to the resolc Wasm ile.
    #[arg(long = "resolc", short, default_value = "resolc")]
    pub resolc: PathBuf,

    /// A list of test corpus JSON files to be tested.
    #[arg(long = "corpus", short)]
    pub corpus: Vec<PathBuf>,

    /// A place to store temporary artifacts during test execution.
    ///
    /// Creates a temporary dir if not specified.
    #[arg(long = "workdir", short)]
    pub working_directory: Option<PathBuf>,

    /// Add a tempdir manually if `working_directory` was not given.
    ///
    /// We attach it here because [TempDir] prunes itself on drop.
    #[clap(skip)]
    #[serde(skip)]
    pub temp_dir: Option<&'static TempDir>,

    /// The path to the `geth` executable.
    ///
    /// By default it uses `geth` binary found in `$PATH`.
    #[arg(short, long = "geth", default_value = "geth")]
    pub geth: PathBuf,

    /// The maximum time in milliseconds to wait for geth to start.
    #[arg(long = "geth-start-timeout", default_value = "10000")]
    pub geth_start_timeout: u64,

    /// The test network chain ID.
    #[arg(short, long = "network-id", default_value = "420420420")]
    pub network_id: u64,

    /// Configure nodes according to this genesis.json file.
    #[arg(long = "genesis", default_value = "genesis.json")]
    pub genesis_file: PathBuf,

    /// The signing account private key.
    #[arg(
        short,
        long = "account",
        default_value = "0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d"
    )]
    pub account: String,

    /// This argument controls which private keys the nodes should have access to and be added to
    /// its wallet signers. With a value of N, private keys (0, N] will be added to the signer set
    /// of the node.
    #[arg(long = "private-keys-count", default_value_t = 100_000)]
    pub private_keys_to_add: usize,

    /// The differential testing leader node implementation.
    #[arg(short, long = "leader", default_value = "geth")]
    pub leader: TestingPlatform,

    /// The differential testing follower node implementation.
    #[arg(short, long = "follower", default_value = "kitchensink")]
    pub follower: TestingPlatform,

    /// Only compile against this testing platform (doesn't execute the tests).
    #[arg(long = "compile-only")]
    pub compile_only: Option<TestingPlatform>,

    /// Determines the amount of nodes that will be spawned for each chain.
    #[arg(long, default_value = "1")]
    pub number_of_nodes: usize,

    /// Determines the amount of tokio worker threads that will will be used.
    #[arg(
        long,
        default_value_t = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    )]
    pub number_of_threads: usize,

    /// Determines the amount of concurrent tasks that will be spawned to run tests. Defaults to 10 x the number of nodes.
    #[arg(long)]
    pub number_concurrent_tasks: Option<usize>,

    /// Extract problems back to the test corpus.
    #[arg(short, long = "extract-problems")]
    pub extract_problems: bool,

    /// The path to the `kitchensink` executable.
    ///
    /// By default it uses `substrate-node` binary found in `$PATH`.
    #[arg(short, long = "kitchensink", default_value = "substrate-node")]
    pub kitchensink: PathBuf,

    /// The path to the `eth_proxy` executable.
    ///
    /// By default it uses `eth-rpc` binary found in `$PATH`.
    #[arg(short = 'p', long = "eth_proxy", default_value = "eth-rpc")]
    pub eth_proxy: PathBuf,
}

impl Arguments {
    /// Return the configured working directory with the following precedence:
    /// 1. `self.working_directory` if it was provided.
    /// 2. `self.temp_dir` if it it was provided
    /// 3. Panic.
    pub fn directory(&self) -> &Path {
        if let Some(path) = &self.working_directory {
            return path.as_path();
        }

        if let Some(temp_dir) = &self.temp_dir {
            return temp_dir.path();
        }

        panic!("should have a workdir configured")
    }

    /// Return the number of concurrent tasks to run. This is provided via the
    /// `--number-concurrent-tasks` argument, and otherwise defaults to --number-of-nodes * 20.
    pub fn number_of_concurrent_tasks(&self) -> usize {
        self.number_concurrent_tasks
            .unwrap_or(20 * self.number_of_nodes)
    }

    /// Try to parse `self.account` into a [PrivateKeySigner],
    /// panicing on error.
    pub fn wallet(&self) -> EthereumWallet {
        let signer = self
            .account
            .parse::<PrivateKeySigner>()
            .unwrap_or_else(|error| {
                panic!("private key '{}' parsing error: {error}", self.account);
            });
        EthereumWallet::new(signer)
    }
}

impl Default for Arguments {
    fn default() -> Self {
        static TEMP_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

        let default = Arguments::parse_from(["retester"]);

        Arguments {
            temp_dir: Some(&TEMP_DIR),
            ..default
        }
    }
}

/// The Solidity compatible node implementation.
///
/// This describes the solutions to be tested against on a high level.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum, Serialize, Deserialize,
)]
#[clap(rename_all = "lower")]
pub enum TestingPlatform {
    /// The go-ethereum reference full node EVM implementation.
    Geth,
    /// The kitchensink runtime provides the PolkaVM (PVM) based node implentation.
    Kitchensink,
}

impl Display for TestingPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Geth => f.write_str("geth"),
            Self::Kitchensink => f.write_str("revive"),
        }
    }
}
