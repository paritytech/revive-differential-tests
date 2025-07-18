//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utility to execute tests.

use revive_dt_compiler::{SolidityCompiler, revive_resolc, solc};
use revive_dt_config::TestingPlatform;
use revive_dt_node::{geth, kitchensink::KitchensinkNode};
use revive_dt_node_interaction::EthereumNode;

pub mod common;
pub mod driver;

/// One platform can be tested differentially against another.
///
/// For this we need a blockchain node implementation and a compiler.
pub trait Platform {
    type Blockchain: EthereumNode;
    type Compiler: SolidityCompiler;

    /// Returns the matching [TestingPlatform] of the [revive_dt_config::Arguments].
    fn config_id() -> TestingPlatform;
}

#[derive(Default)]
pub struct Geth;

impl Platform for Geth {
    type Blockchain = geth::Instance;
    type Compiler = solc::Solc;

    fn config_id() -> TestingPlatform {
        TestingPlatform::Geth
    }
}

#[derive(Default)]
pub struct Kitchensink;

impl Platform for Kitchensink {
    type Blockchain = KitchensinkNode;
    type Compiler = revive_resolc::Resolc;

    fn config_id() -> TestingPlatform {
        TestingPlatform::Kitchensink
    }
}
