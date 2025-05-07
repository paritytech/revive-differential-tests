//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utilty to execute tests.

use revive_dt_compiler::{SolidityCompiler, solc, revive_resolc};
use revive_dt_config::Arguments;
use revive_dt_node::geth;
use revive_dt_node_interaction::EthereumNode;
use revive_dt_solc_binaries::download_solc;

use std::path::PathBuf;
use semver::Version;

pub mod driver;

/// One platform can be tested differentially against another.
///
/// For this we need a blockchain node implementation and a compiler.
pub trait Platform {
    type Blockchain: EthereumNode;
    type Compiler: SolidityCompiler;

    fn get_compiler_executable(config: &Arguments, version: Version) -> anyhow::Result<PathBuf>;
}

#[derive(Default)]
pub struct Geth;

impl Platform for Geth {
    type Blockchain = geth::Instance;
    type Compiler = solc::Solc;
    
    fn get_compiler_executable(config: &Arguments, version: Version) -> anyhow::Result<PathBuf> {

        let path = download_solc(config.directory(), version, config.wasm)?;
        Ok(path)
    }
}

#[derive(Default)]
pub struct Kitchensink;

impl Platform for Kitchensink {
    type Blockchain = geth::Instance;
    type Compiler = revive_resolc::Resolc;
    
    fn get_compiler_executable(config: &Arguments, _version: Version) -> anyhow::Result<PathBuf> {

        if !config.resolc.as_os_str().is_empty() {
            return Ok(config.resolc.clone());
        }

        Ok(PathBuf::from("resolc"))
    }
}
