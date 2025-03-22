//! The global configuration used accross all revive differential testing crates.

use std::{path::PathBuf, sync::OnceLock};

use clap::Parser;

static ARGUMENTS: OnceLock<Arguments> = OnceLock::new();

/// Get the command line arguments.
pub fn get_args() -> &'static Arguments {
    ARGUMENTS.get_or_init(Arguments::parse)
}

#[derive(Debug, Parser, Clone)]
#[command(
    name = "revive compiler differential tester utility",
    arg_required_else_help = true
)]
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
    #[arg(long = "workdir", short)]
    pub working_directory: PathBuf,

    /// The path to the `geth` executable.
    ///
    /// By default it uses `geth` binary found in `$PATH`.
    #[arg(short, long = "geth", default_value = "geth")]
    pub geth: PathBuf,
}
