//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utility to execute tests.

use std::{
    pin::Pin,
    thread::{self, JoinHandle},
};

use alloy::genesis::Genesis;
use anyhow::Context as _;
use revive_dt_common::types::*;
use revive_dt_compiler::{
    DynSolidityCompiler, SolidityCompiler, revive_resolc,
    solc::{self, Solc},
};
use revive_dt_config::*;
use revive_dt_format::traits::ResolverApi;
use revive_dt_node::{
    Node,
    geth::{self, GethNode},
    kitchensink::KitchensinkNode,
};
use revive_dt_node_interaction::EthereumNode;
use tracing::info;

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
#[allow(clippy::type_complexity)]
pub trait DynPlatform {
    /// Returns the identifier of this platform. This is a combination of the node and the compiler
    /// used.
    fn platform_identifier(&self) -> PlatformIdentifier;

    /// Returns a full identifier for the platform.
    fn full_identifier(&self) -> (NodeIdentifier, VmIdentifier, CompilerIdentifier) {
        (
            self.node_identifier(),
            self.vm_identifier(),
            self.compiler_identifier(),
        )
    }

    /// Returns the identifier of the node used.
    fn node_identifier(&self) -> NodeIdentifier;

    /// Returns the identifier of the vm used.
    fn vm_identifier(&self) -> VmIdentifier;

    /// Returns the identifier of the compiler used.
    fn compiler_identifier(&self) -> CompilerIdentifier;

    /// Creates a new node for the platform by spawning a new thread, creating the node object,
    /// initializing it, spawning it, and waiting for it to start up.
    fn new_node(
        &self,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<ConcurrencyConfiguration>
        + AsRef<GenesisConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<GethConfiguration>
        + AsRef<KitchensinkConfiguration>
        + AsRef<ReviveDevNodeConfiguration>
        + AsRef<EthRpcConfiguration>
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>>;

    /// Creates a new compiler for the provided platform
    fn new_compiler<'a>(
        &self,
        context: impl AsRef<SolcConfiguration>
        + AsRef<ResolcConfiguration>
        + AsRef<WorkingDirectoryConfiguration>
        + 'a,
        version: impl Into<Option<VersionOrRequirement>> + 'a,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn DynSolidityCompiler>>> + 'a>>;
}

pub struct GethEvmPlatform;

impl DynPlatform for GethEvmPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::GethEvmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::Geth
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }

    fn new_node(
        &self,
        context: impl AsRef<WorkingDirectoryConfiguration>
        + AsRef<ConcurrencyConfiguration>
        + AsRef<GenesisConfiguration>
        + AsRef<WalletConfiguration>
        + AsRef<GethConfiguration>
        + AsRef<KitchensinkConfiguration>
        + AsRef<ReviveDevNodeConfiguration>
        + AsRef<EthRpcConfiguration>
        + Send
        + Sync
        + Clone
        + 'static,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = GethNode::new(context);
            let node = spawn_node::<GethNode>(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler<'a>(
        &self,
        context: impl AsRef<SolcConfiguration>
        + AsRef<ResolcConfiguration>
        + AsRef<WorkingDirectoryConfiguration>
        + 'a,
        version: impl Into<Option<VersionOrRequirement>> + 'a,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn DynSolidityCompiler>>> + 'a>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn DynSolidityCompiler>)
        })
    }
}

fn spawn_node<T: Node + EthereumNode + Send + Sync>(
    mut node: T,
    genesis: Genesis,
) -> anyhow::Result<T> {
    info!(
        id = node.id(),
        connection_string = node.connection_string(),
        "Spawning node"
    );
    node.spawn(genesis)
        .context("Failed to spawn node process")?;
    info!(
        id = node.id(),
        connection_string = node.connection_string(),
        "Spawned node"
    );
    Ok(node)
}
