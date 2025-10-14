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
use revive_dt_compiler::{SolidityCompiler, revive_resolc::Resolc, solc::Solc};
use revive_dt_config::*;
use revive_dt_node::{
    Node, node_implementations::geth::GethNode,
    node_implementations::lighthouse_geth::LighthouseGethNode,
    node_implementations::substrate::SubstrateNode, node_implementations::zombienet::ZombienetNode,
};
use revive_dt_node_interaction::EthereumNode;
use tracing::info;

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
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>>;

    /// Creates a new compiler for the provided platform
    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>>;

    /// Exports the genesis/chainspec for the node.
    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value>;
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = GethNode::new(context);
            let node = spawn_node::<GethNode>(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let genesis = AsRef::<GenesisConfiguration>::as_ref(&context).genesis()?;
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        let node_genesis = GethNode::node_genesis(genesis.clone(), &wallet);
        serde_json::to_value(node_genesis)
            .context("Failed to convert node genesis to a serde_value")
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = LighthouseGethNode::new(context);
            let node = spawn_node::<LighthouseGethNode>(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let genesis = AsRef::<GenesisConfiguration>::as_ref(&context).genesis()?;
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        let node_genesis = LighthouseGethNode::node_genesis(genesis.clone(), &wallet);
        serde_json::to_value(node_genesis)
            .context("Failed to convert node genesis to a serde_value")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct KitchensinkPolkavmResolcPlatform;

impl Platform for KitchensinkPolkavmResolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::KitchensinkPolkavmResolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::Kitchensink
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::PolkaVM
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Resolc
    }

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let kitchensink_path = AsRef::<KitchensinkConfiguration>::as_ref(&context)
            .path
            .clone();
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = SubstrateNode::new(
                kitchensink_path,
                SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND,
                None,
                context,
                &[],
            );
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Resolc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let kitchensink_path = AsRef::<KitchensinkConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();
        let export_chainspec_command = SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND;

        SubstrateNode::node_genesis(kitchensink_path, export_chainspec_command, &wallet)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct KitchensinkRevmSolcPlatform;

impl Platform for KitchensinkRevmSolcPlatform {
    fn platform_identifier(&self) -> PlatformIdentifier {
        PlatformIdentifier::KitchensinkRevmSolc
    }

    fn node_identifier(&self) -> NodeIdentifier {
        NodeIdentifier::Kitchensink
    }

    fn vm_identifier(&self) -> VmIdentifier {
        VmIdentifier::Evm
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        CompilerIdentifier::Solc
    }

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let kitchensink_path = AsRef::<KitchensinkConfiguration>::as_ref(&context)
            .path
            .clone();
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = SubstrateNode::new(
                kitchensink_path,
                SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND,
                None,
                context,
                &[],
            );
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let kitchensink_path = AsRef::<KitchensinkConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();
        let export_chainspec_command = SubstrateNode::KITCHENSINK_EXPORT_CHAINSPEC_COMMAND;

        SubstrateNode::node_genesis(kitchensink_path, export_chainspec_command, &wallet)
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let revive_dev_node_configuration = AsRef::<ReviveDevNodeConfiguration>::as_ref(&context);
        let eth_rpc_configuration = AsRef::<EthRpcConfiguration>::as_ref(&context);

        let revive_dev_node_path = revive_dev_node_configuration.path.clone();
        let revive_dev_node_consensus = revive_dev_node_configuration.consensus.clone();

        let eth_rpc_connection_strings = eth_rpc_configuration.existing_rpc_url.clone();

        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = SubstrateNode::new(
                revive_dev_node_path,
                SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND,
                Some(revive_dev_node_consensus),
                context,
                &eth_rpc_connection_strings,
            );
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Resolc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let revive_dev_node_path = AsRef::<ReviveDevNodeConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();
        let export_chainspec_command = SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND;

        SubstrateNode::node_genesis(revive_dev_node_path, export_chainspec_command, &wallet)
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let revive_dev_node_configuration = AsRef::<ReviveDevNodeConfiguration>::as_ref(&context);
        let eth_rpc_configuration = AsRef::<EthRpcConfiguration>::as_ref(&context);

        let revive_dev_node_path = revive_dev_node_configuration.path.clone();
        let revive_dev_node_consensus = revive_dev_node_configuration.consensus.clone();

        let eth_rpc_connection_strings = eth_rpc_configuration.existing_rpc_url.clone();

        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = SubstrateNode::new(
                revive_dev_node_path,
                SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND,
                Some(revive_dev_node_consensus),
                context,
                &eth_rpc_connection_strings,
            );
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let revive_dev_node_path = AsRef::<ReviveDevNodeConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();
        let export_chainspec_command = SubstrateNode::REVIVE_DEV_NODE_EXPORT_CHAINSPEC_COMMAND;

        SubstrateNode::node_genesis(revive_dev_node_path, export_chainspec_command, &wallet)
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let polkadot_parachain_path = AsRef::<PolkadotParachainConfiguration>::as_ref(&context)
            .path
            .clone();
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = ZombienetNode::new(polkadot_parachain_path, context);
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Resolc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let polkadot_parachain_path = AsRef::<PolkadotParachainConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        ZombienetNode::node_genesis(polkadot_parachain_path, &wallet)
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

    fn new_node(
        &self,
        context: Context,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<Box<dyn EthereumNode + Send + Sync>>>> {
        let genesis_configuration = AsRef::<GenesisConfiguration>::as_ref(&context);
        let polkadot_parachain_path = AsRef::<PolkadotParachainConfiguration>::as_ref(&context)
            .path
            .clone();
        let genesis = genesis_configuration.genesis()?.clone();
        Ok(thread::spawn(move || {
            let node = ZombienetNode::new(polkadot_parachain_path, context);
            let node = spawn_node(node, genesis)?;
            Ok(Box::new(node) as Box<_>)
        }))
    }

    fn new_compiler(
        &self,
        context: Context,
        version: Option<VersionOrRequirement>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn SolidityCompiler>>>>> {
        Box::pin(async move {
            let compiler = Solc::new(context, version).await;
            compiler.map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
        })
    }

    fn export_genesis(&self, context: Context) -> anyhow::Result<serde_json::Value> {
        let polkadot_parachain_path = AsRef::<PolkadotParachainConfiguration>::as_ref(&context)
            .path
            .as_path();
        let wallet = AsRef::<WalletConfiguration>::as_ref(&context).wallet();

        ZombienetNode::node_genesis(polkadot_parachain_path, &wallet)
    }
}

impl From<PlatformIdentifier> for Box<dyn Platform> {
    fn from(value: PlatformIdentifier) -> Self {
        match value {
            PlatformIdentifier::GethEvmSolc => Box::new(GethEvmSolcPlatform) as Box<_>,
            PlatformIdentifier::LighthouseGethEvmSolc => {
                Box::new(LighthouseGethEvmSolcPlatform) as Box<_>
            }
            PlatformIdentifier::KitchensinkPolkavmResolc => {
                Box::new(KitchensinkPolkavmResolcPlatform) as Box<_>
            }
            PlatformIdentifier::KitchensinkRevmSolc => {
                Box::new(KitchensinkRevmSolcPlatform) as Box<_>
            }
            PlatformIdentifier::ReviveDevNodePolkavmResolc => {
                Box::new(ReviveDevNodePolkavmResolcPlatform) as Box<_>
            }
            PlatformIdentifier::ReviveDevNodeRevmSolc => {
                Box::new(ReviveDevNodeRevmSolcPlatform) as Box<_>
            }
            PlatformIdentifier::ZombienetPolkavmResolc => {
                Box::new(ZombienetPolkavmResolcPlatform) as Box<_>
            }
            PlatformIdentifier::ZombienetRevmSolc => Box::new(ZombienetRevmSolcPlatform) as Box<_>,
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
            PlatformIdentifier::KitchensinkPolkavmResolc => {
                &KitchensinkPolkavmResolcPlatform as &dyn Platform
            }
            PlatformIdentifier::KitchensinkRevmSolc => {
                &KitchensinkRevmSolcPlatform as &dyn Platform
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
        }
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
