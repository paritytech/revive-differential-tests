use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "The PolkaVM Solidity compiler", arg_required_else_help = true)]
pub struct Arguments {
    /// The path where the `resolc` executable to be tested is found at.
    ///
    /// By default it uses the `resolc` found in `$PATH`
    #[arg(long = "resolc")]
    pub resolc: Option<PathBuf>,

    /// A list of test corpus JSON files to be tested.
    #[arg(long = "corpus")]
    pub corpus: Vec<PathBuf>,
}
