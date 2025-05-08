//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utilty to execute tests.

use revive_dt_compiler::{SolidityCompiler, revive_resolc, solc};
use revive_dt_node::geth;
use revive_dt_node_interaction::EthereumNode;

pub mod driver;

/// One platform can be tested differentially against another.
///
/// For this we need a blockchain node implementation and a compiler.
pub trait Platform {
    type Blockchain: EthereumNode;
    type Compiler: SolidityCompiler;
}

#[derive(Default)]
pub struct Geth;

impl Platform for Geth {
    type Blockchain = geth::Instance;
    type Compiler = solc::Solc;
}

#[derive(Default)]
pub struct Kitchensink;

impl Platform for Kitchensink {
    type Blockchain = geth::Instance;
    type Compiler = revive_resolc::Resolc;
}
