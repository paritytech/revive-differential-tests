//! The global configuration used accross all revive differential testing crates.

use std::{env, path::PathBuf};

use clap::Parser;

#[derive(Debug, Parser, Clone)]
#[command(name = "retester")]
pub struct Arguments {
    /// The path to the `resolc` executable to be tested.
    ///
    /// By default it uses the `resolc` binary found in `$PATH`.
    #[arg(long = "resolc", short, default_value = "resolc")]
    pub resolc: PathBuf,

    /// A list of test corpus JSON files to be tested.
    #[arg(long = "corpus", short)]
    pub corpus: Vec<PathBuf>,

    /// A place to store temporary artifacts during test execution.
    #[arg(long = "workdir", short, default_value_t = cwd())]
    pub working_directory: String,

    /// The path to the `geth` executable.
    ///
    /// By default it uses `geth` binary found in `$PATH`.
    #[arg(short, long = "geth", default_value = "geth")]
    pub geth: PathBuf,

    /// The maximum time in milliseconds to wait for geth to start.
    #[arg(long = "geth-start-timeout", default_value = "2000")]
    pub geth_start_timeout: u64,

    /// The test network chain ID.
    #[arg(short, long = "network-id", default_value = "420420420")]
    pub network_id: u64,

    /// Configure nodes according to this genesis.json file.
    #[arg(long = "genesis-file")]
    pub genesis_file: Option<PathBuf>,
}

fn cwd() -> String {
    env::current_dir()
        .expect("should be able to access current woring directory")
        .to_string_lossy()
        .to_string()
}

impl Default for Arguments {
    fn default() -> Self {
        Arguments::parse_from(["retester"])
    }
}
