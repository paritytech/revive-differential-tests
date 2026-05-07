//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utility to execute tests.

pub mod prelude {
    pub use crate::Platform;
    pub use crate::{
        GethEvmSolcPlatform, LighthouseGethEvmSolcPlatform, PolkadotOmniNodePolkavmResolcPlatform,
        PolkadotOmniNodeRevmSolcPlatform, ReviveDevNodePolkavmResolcPlatform,
        ReviveDevNodeRevmSolcPlatform, ZombienetPolkavmResolcPlatform, ZombienetRevmSolcPlatform,
    };
}

pub(crate) mod internal_prelude {
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_compiler::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_node::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::thread::{self, JoinHandle};

    pub use alloy::genesis::Genesis;
    pub use anyhow::{Context as _, Result};
    pub use serde_json;
    pub use tracing::info;
}

use crate::internal_prelude::*;

/// A trait that describes the interface for the platforms that are supported by the tool.
#[allow(clippy::type_complexity)]
pub trait Platform {
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
        context: Context,
    ) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
        match self.node_identifier() {
            NodeIdentifier::Geth => new_geth_node(context),
            NodeIdentifier::LighthouseGeth => new_lighthouse_geth_node(context),
            NodeIdentifier::ReviveDevNode => new_revive_dev_node(context),
            NodeIdentifier::Zombienet => {
                #[cfg(unix)]
                {
                    new_zombienet_node(context)
                }
                #[cfg(not(unix))]
                {
                    anyhow::bail!("Zombienet is not supported on this platform")
                }
            }
            NodeIdentifier::PolkadotOmniNode => new_polkadot_omni_node(context),
        }
    }

    /// Creates a new compiler for the provided platform.
    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> FrameworkFuture<Result<Box<dyn SolidityCompiler + Send + Sync>>> {
        match self.compiler_identifier() {
            CompilerIdentifier::Solc => new_solc_compiler(context, version),
            CompilerIdentifier::Resolc => new_resolc_compiler(context, version),
        }
    }

    /// Exports the genesis/chainspec for the node.
    fn export_genesis(&self, context: Context) -> Result<serde_json::Value> {
        match self.node_identifier() {
            NodeIdentifier::Geth => export_geth_genesis(context),
            NodeIdentifier::LighthouseGeth => export_lighthouse_geth_genesis(context),
            NodeIdentifier::ReviveDevNode => export_revive_dev_node_genesis(context),
            NodeIdentifier::Zombienet => {
                #[cfg(unix)]
                {
                    export_zombienet_genesis(context)
                }
                #[cfg(not(unix))]
                {
                    anyhow::bail!("Zombienet is not supported on this platform")
                }
            }
            NodeIdentifier::PolkadotOmniNode => export_polkadot_omni_node_genesis(context),
        }
    }

    /// Describes if the platform allows for the gas fees to be cached.
    fn allow_caching_gas_limit(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct GethEvmSolcPlatform;

impl Platform for GethEvmSolcPlatform {
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct LighthouseGethEvmSolcPlatform;

impl Platform for LighthouseGethEvmSolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::LighthouseGethEvmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::LighthouseGeth
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct ReviveDevNodePolkavmResolcPlatform;

impl Platform for ReviveDevNodePolkavmResolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::ReviveDevNodePolkavmResolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::ReviveDevNode
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::PolkaVM
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Resolc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct ReviveDevNodeRevmSolcPlatform;

impl Platform for ReviveDevNodeRevmSolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::ReviveDevNodeRevmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::ReviveDevNode
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct ZombienetPolkavmResolcPlatform;

impl Platform for ZombienetPolkavmResolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::ZombienetPolkavmResolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::Zombienet
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::PolkaVM
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Resolc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct ZombienetRevmSolcPlatform;

impl Platform for ZombienetRevmSolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::ZombienetRevmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::Zombienet
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct PolkadotOmniNodePolkavmResolcPlatform;

impl Platform for PolkadotOmniNodePolkavmResolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::PolkadotOmniNodePolkavmResolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::PolkadotOmniNode
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::PolkaVM
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Resolc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct PolkadotOmniNodeRevmSolcPlatform;

impl Platform for PolkadotOmniNodeRevmSolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::PolkadotOmniNodeRevmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::PolkadotOmniNode
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }
}

impl From<PlatformIdentifier> for Box<dyn Platform> {
    fn from(value: PlatformIdentifier) -> Self {
        match value {
            PlatformIdentifier::GethEvmSolc => Box::new(GethEvmSolcPlatform) as _,
            PlatformIdentifier::LighthouseGethEvmSolc => {
                Box::new(LighthouseGethEvmSolcPlatform) as _
            }
            PlatformIdentifier::ReviveDevNodePolkavmResolc => {
                Box::new(ReviveDevNodePolkavmResolcPlatform) as _
            }
            PlatformIdentifier::ReviveDevNodeRevmSolc => {
                Box::new(ReviveDevNodeRevmSolcPlatform) as _
            }
            PlatformIdentifier::ZombienetPolkavmResolc => {
                Box::new(ZombienetPolkavmResolcPlatform) as _
            }
            PlatformIdentifier::ZombienetRevmSolc => Box::new(ZombienetRevmSolcPlatform) as _,
            PlatformIdentifier::PolkadotOmniNodePolkavmResolc => {
                Box::new(PolkadotOmniNodePolkavmResolcPlatform) as _
            }
            PlatformIdentifier::PolkadotOmniNodeRevmSolc => {
                Box::new(PolkadotOmniNodeRevmSolcPlatform) as _
            }
        }
    }
}

impl From<PlatformIdentifier> for &dyn Platform {
    fn from(value: PlatformIdentifier) -> Self {
        match value {
            PlatformIdentifier::GethEvmSolc => &GethEvmSolcPlatform as &dyn Platform,
            PlatformIdentifier::LighthouseGethEvmSolc => {
                &LighthouseGethEvmSolcPlatform as &dyn Platform
            }
            PlatformIdentifier::ReviveDevNodePolkavmResolc => {
                &ReviveDevNodePolkavmResolcPlatform as &dyn Platform
            }
            PlatformIdentifier::ReviveDevNodeRevmSolc => {
                &ReviveDevNodeRevmSolcPlatform as &dyn Platform
            }
            PlatformIdentifier::ZombienetPolkavmResolc => {
                &ZombienetPolkavmResolcPlatform as &dyn Platform
            }
            PlatformIdentifier::ZombienetRevmSolc => &ZombienetRevmSolcPlatform as &dyn Platform,
            PlatformIdentifier::PolkadotOmniNodePolkavmResolc => {
                &PolkadotOmniNodePolkavmResolcPlatform as &dyn Platform
            }
            PlatformIdentifier::PolkadotOmniNodeRevmSolc => {
                &PolkadotOmniNodeRevmSolcPlatform as &dyn Platform
            }
        }
    }
}

fn new_geth_node(context: Context) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
    let genesis = context.as_genesis_configuration().genesis()?.clone();
    Ok(thread::spawn(move || {
        let use_fallback_gas_filler = matches!(context, Context::Test(..));
        let node = GethNode::new(context, use_fallback_gas_filler);
        let node = spawn_node(node, genesis)?;
        Ok(Box::new(node) as _)
    }))
}

fn new_lighthouse_geth_node(
    context: Context,
) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
    let genesis = context.as_genesis_configuration().genesis()?.clone();
    Ok(thread::spawn(move || {
        let use_fallback_gas_filler = matches!(context, Context::Test(..));
        let node = LighthouseGethNode::new(context, use_fallback_gas_filler);
        let node = spawn_node(node, genesis)?;
        Ok(Box::new(node) as _)
    }))
}

fn new_revive_dev_node(
    context: Context,
) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
    let revive_dev_node_configuration = context.as_revive_dev_node_configuration();
    let eth_rpc_configuration = context.as_eth_rpc_configuration();

    let revive_dev_node_path = revive_dev_node_configuration.path.clone();
    let revive_dev_node_consensus = revive_dev_node_configuration.consensus.clone();
    let eth_rpc_connection_strings = revive_dev_node_configuration.existing_rpc_url.clone();
    let node_logging_level = revive_dev_node_configuration.logging_level.clone();
    let eth_rpc_logging_level = eth_rpc_configuration.logging_level.clone();

    let genesis = context.as_genesis_configuration().genesis()?.clone();
    Ok(thread::spawn(move || {
        let use_fallback_gas_filler = matches!(context, Context::Test(..));
        let node = SubstrateNode::new(
            revive_dev_node_path,
            SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND,
            Some(revive_dev_node_consensus),
            context,
            &eth_rpc_connection_strings,
            use_fallback_gas_filler,
            node_logging_level,
            eth_rpc_logging_level,
        );
        let node = spawn_node(node, genesis)?;
        Ok(Box::new(node) as _)
    }))
}

#[cfg(unix)]
fn new_zombienet_node(
    context: Context,
) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
    let polkadot_parachain_path = context.as_polkadot_parachain_configuration().path.clone();
    let genesis = context.as_genesis_configuration().genesis()?.clone();
    Ok(thread::spawn(move || {
        let use_fallback_gas_filler = matches!(context, Context::Test(..));
        let node = ZombienetNode::new(polkadot_parachain_path, context, use_fallback_gas_filler);
        let node = spawn_node(node, genesis)?;
        Ok(Box::new(node) as _)
    }))
}

fn new_polkadot_omni_node(
    context: Context,
) -> Result<JoinHandle<Result<Box<dyn NodeApi + Send + Sync>>>> {
    let genesis = context.as_genesis_configuration().genesis()?.clone();
    Ok(thread::spawn(move || {
        let use_fallback_gas_filler = matches!(context, Context::Test(..));
        let node = PolkadotOmnichainNode::new(context, use_fallback_gas_filler);
        let node = spawn_node(node, genesis)?;
        Ok(Box::new(node) as _)
    }))
}

fn new_solc_compiler(
    context: Context,
    version: Option<VersionOrRequirement>,
) -> FrameworkFuture<Result<Box<dyn SolidityCompiler + Send + Sync>>> {
    Box::pin(async move {
        let compiler = Solc::new_native(context, version).await;
        compiler.map(|compiler| Box::new(compiler) as _)
    })
}

fn new_resolc_compiler(
    context: Context,
    version: Option<VersionOrRequirement>,
) -> FrameworkFuture<Result<Box<dyn SolidityCompiler + Send + Sync>>> {
    Box::pin(async move {
        let compiler = Resolc::new(context, version).await;
        compiler.map(|compiler| Box::new(compiler) as _)
    })
}

fn export_geth_genesis(context: Context) -> Result<serde_json::Value> {
    let genesis = context.as_genesis_configuration().genesis()?;
    let wallet = context.as_wallet_configuration().wallet();
    let node_genesis = GethNode::node_genesis(genesis.clone(), &wallet);
    serde_json::to_value(node_genesis).context("Failed to convert node genesis to a serde_value")
}

fn export_lighthouse_geth_genesis(context: Context) -> Result<serde_json::Value> {
    let genesis = context.as_genesis_configuration().genesis()?;
    let wallet = context.as_wallet_configuration().wallet();
    let node_genesis = LighthouseGethNode::node_genesis(genesis.clone(), &wallet);
    serde_json::to_value(node_genesis).context("Failed to convert node genesis to a serde_value")
}

fn export_revive_dev_node_genesis(context: Context) -> Result<serde_json::Value> {
    let revive_dev_node_path = context.as_revive_dev_node_configuration().path.as_path();
    let wallet = context.as_wallet_configuration().wallet();
    let export_chainspec_command = SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND;
    SubstrateNode::node_genesis(revive_dev_node_path, export_chainspec_command, &wallet)
}

#[cfg(unix)]
fn export_zombienet_genesis(context: Context) -> Result<serde_json::Value> {
    let polkadot_parachain_path = context.as_polkadot_parachain_configuration().path.as_path();
    let wallet = context.as_wallet_configuration().wallet();
    ZombienetNode::node_genesis(polkadot_parachain_path, &wallet)
}

fn export_polkadot_omni_node_genesis(context: Context) -> Result<serde_json::Value> {
    let config = context.as_polkadot_omnichain_node_configuration();
    let wallet = context.as_wallet_configuration().wallet();
    PolkadotOmnichainNode::node_genesis(
        &wallet,
        config
            .chain_spec_path
            .as_ref()
            .context("No WASM runtime path found in the polkadot-omni-node configuration")?,
    )
}

fn spawn_node<T: Node + NodeApi + Send + Sync>(mut node: T, genesis: Genesis) -> Result<T> {
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
