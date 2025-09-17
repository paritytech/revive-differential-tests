//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utility to execute tests.

use revive_dt_compiler::{SolidityCompiler, revive_resolc, solc};
use revive_dt_config::{PlatformIdentifier, TestingPlatform};
use revive_dt_format::traits::ResolverApi;
use revive_dt_node::{Node, geth, kitchensink::KitchensinkNode};
use revive_dt_node_interaction::EthereumNode;

pub mod driver;

/// One platform can be tested differentially against another.
///
/// For this we need a blockchain node implementation and a compiler.
pub trait Platform {
    type Blockchain: EthereumNode + Node + ResolverApi;
    type Compiler: SolidityCompiler;

    /// Returns the matching [TestingPlatform] of the [revive_dt_config::Arguments].
    fn config_id() -> &'static TestingPlatform;
}

#[derive(Default)]
pub struct Geth;

impl Platform for Geth {
    type Blockchain = geth::GethNode;
    type Compiler = solc::Solc;

    fn config_id() -> &'static TestingPlatform {
        &TestingPlatform::Geth
    }
}

#[derive(Default)]
pub struct Kitchensink;

impl Platform for Kitchensink {
    type Blockchain = KitchensinkNode;
    type Compiler = revive_resolc::Resolc;

    fn config_id() -> &'static TestingPlatform {
        &TestingPlatform::Kitchensink
    }
}

/// A trait that describes the interface for the platforms that are supported by the tool.
pub trait DynPlatform {
    /// Returns the identifier of this platform.
    fn platform_identifier(&self) -> PlatformIdentifier;

    /// Creates a new node for the platform by spawning a new thread, creating the node object,
    /// initializing it, spawning it, and waiting for it to start up.
    fn new_node(&self) -> Box<dyn PlatformNode>;
}
