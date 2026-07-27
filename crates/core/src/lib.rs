//! The revive differential testing core library.
//!
//! This crate defines the testing configuration and
//! provides a helper utility to execute tests.

pub mod prelude {
    pub use crate::{
        GethEvmSolcPlatform, LighthouseGethEvmSolcPlatform, Platform, PlatformDescriptorWithName,
        PolkadotOmniNodePolkavmResolcPlatform, PolkadotOmniNodeRevmSolcPlatform,
        ReviveDevNodePolkavmResolcPlatform, ReviveDevNodeRevmSolcPlatform,
        ZombienetPolkavmResolcPlatform, ZombienetRevmSolcPlatform,
    };
}

pub(crate) mod internal_prelude {
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_compiler::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_node::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::sync::LazyLock;

    pub use anyhow::{Context as _, Result};
    #[cfg(not(unix))]
    pub use futures::{FutureExt as _, future::ready};
}

use crate::internal_prelude::*;

/// A trait that describes the interface for the platforms that are supported by the tool.
#[allow(clippy::type_complexity)]
pub trait Platform {
    /// Returns the name of this platform.
    fn platform_name(&self) -> &PlatformName;

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

    /// Creates and initializes a new node for the platform.
    fn new_node(&self, context: Context) -> StaticFuture<Result<NodeConnector>> {
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
                    ready(Err(anyhow::anyhow!(
                        "Zombienet is not supported on this platform"
                    )))
                    .boxed()
                }
            }
            NodeIdentifier::PolkadotOmniNode => new_polkadot_omni_node(context),
        }
    }

    /// Creates a new compiler for the provided platform.
    fn new_compiler(
        &self,
        cargo_configuration: &CargoConfiguration,
        solc_configuration: &SolcConfiguration,
        resolc_configuration: &ResolcConfiguration,
        working_directory_configuration: &WorkingDirectoryConfiguration,
        version: Option<VersionOrRequirement>,
    ) -> StaticFuture<Result<Box<dyn ContractCompiler + Send + Sync>>> {
        match self.compiler_identifier() {
            CompilerIdentifier::Cargo => new_cargo_compiler(
                cargo_configuration.clone(),
                working_directory_configuration.clone(),
            ),
            CompilerIdentifier::Solc => new_solc_compiler(
                solc_configuration.clone(),
                working_directory_configuration.clone(),
                version,
            ),
            CompilerIdentifier::Resolc => new_resolc_compiler(
                solc_configuration.clone(),
                resolc_configuration.clone(),
                working_directory_configuration.clone(),
                version,
            ),
        }
    }
}

/// A configured platform paired with the name used to identify it.
#[derive(Clone, Debug)]
pub struct PlatformDescriptorWithName {
    /// The name used to identify the platform.
    pub name: PlatformName,
    /// The node and compiler configuration for the platform.
    pub descriptor: PlatformDescriptor,
}

impl Platform for PlatformDescriptorWithName {
    fn platform_name(&self) -> &PlatformName {
        &self.name
    }

    fn node_identifier(&self) -> NodeIdentifier {
        match &self.descriptor.node {
            AnyNodeConfiguration::Zombienet(_) => NodeIdentifier::Zombienet,
            AnyNodeConfiguration::Geth(_) => NodeIdentifier::Geth,
            AnyNodeConfiguration::Kurtosis(_) => NodeIdentifier::LighthouseGeth,
            AnyNodeConfiguration::ReviveDevNode(_) => NodeIdentifier::ReviveDevNode,
            AnyNodeConfiguration::PolkadotOmnichainNode(_) => NodeIdentifier::PolkadotOmniNode,
        }
    }

    fn vm_identifier(&self) -> VmIdentifier {
        match &self.descriptor.compiler {
            AnyCompilerConfiguration::Solc(_) => VmIdentifier::Evm,
            AnyCompilerConfiguration::Cargo(_) | AnyCompilerConfiguration::Resolc { .. } => {
                VmIdentifier::PolkaVM
            }
        }
    }

    fn compiler_identifier(&self) -> CompilerIdentifier {
        match &self.descriptor.compiler {
            AnyCompilerConfiguration::Cargo(_) => CompilerIdentifier::Cargo,
            AnyCompilerConfiguration::Solc(_) => CompilerIdentifier::Solc,
            AnyCompilerConfiguration::Resolc { .. } => CompilerIdentifier::Resolc,
        }
    }

    fn new_node(&self, context: Context) -> StaticFuture<Result<NodeConnector>> {
        let node_configuration = self.descriptor.node.clone();
        let eth_rpc_configuration = self.descriptor.eth_rpc.clone();

        Box::pin(async move {
            let (working_directory_configuration, wallet_configuration, subscription_kind) =
                match &context {
                    Context::Test(context) => (
                        &context.working_directory,
                        &context.wallet,
                        BlockProvisioningSubscriptionKind::BestBlocks,
                    ),
                    Context::Benchmark(context) => (
                        &context.working_directory,
                        &context.wallet,
                        BlockProvisioningSubscriptionKind::FinalizedBlocks,
                    ),
                    Context::ExportJsonSchema(_)
                    | Context::ExportTestSpecifiers(_)
                    | Context::Compile(_) => {
                        anyhow::bail!("Nodes can only be created for tests and benchmarks")
                    }
                };
            let connector_configurations = match &node_configuration {
                AnyNodeConfiguration::Zombienet(configuration) => {
                    configuration.connector_configurations.as_deref()
                }
                AnyNodeConfiguration::Geth(configuration) => {
                    configuration.connector_configurations.as_deref()
                }
                AnyNodeConfiguration::Kurtosis(configuration) => {
                    configuration.connector_configurations.as_deref()
                }
                AnyNodeConfiguration::ReviveDevNode(configuration) => {
                    configuration.connector_configurations.as_deref()
                }
                AnyNodeConfiguration::PolkadotOmnichainNode(configuration) => {
                    configuration.connector_configurations.as_deref()
                }
            };
            let node_configurations =
                node_configurations(subscription_kind, connector_configurations)
                    .context("Failed to parse the platform's node connector configuration")?;
            let wallet = wallet_configuration.wallet();

            match node_configuration {
                AnyNodeConfiguration::Zombienet(zombienet_configuration) => {
                    #[cfg(unix)]
                    {
                        let eth_rpc_configuration = eth_rpc_configuration
                            .as_ref()
                            .context("Zombienet requires an eth-rpc configuration")?;
                        let node = tokio::task::block_in_place(|| {
                            ZombienetNode::new(
                                working_directory_configuration,
                                eth_rpc_configuration,
                                wallet_configuration,
                                &zombienet_configuration,
                            )
                        })
                        .context("Failed to spawn zombienet")?;
                        NodeConnector::new(node, wallet, node_configurations).await
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = zombienet_configuration;
                        anyhow::bail!("Zombienet is not supported on this platform")
                    }
                }
                AnyNodeConfiguration::Geth(geth_configuration) => {
                    let node = GethNode::new(
                        working_directory_configuration,
                        wallet_configuration,
                        &geth_configuration,
                    )
                    .context("Failed to spawn geth node")?;
                    NodeConnector::new(node, wallet, node_configurations).await
                }
                AnyNodeConfiguration::Kurtosis(kurtosis_configuration) => {
                    let node = LighthouseGethNode::new(
                        working_directory_configuration,
                        wallet_configuration,
                        &kurtosis_configuration,
                    )
                    .context("Failed to spawn lighthouse node")?;
                    NodeConnector::new(node, wallet, node_configurations).await
                }
                AnyNodeConfiguration::ReviveDevNode(revive_dev_node_configuration) => {
                    let eth_rpc_configuration = eth_rpc_configuration
                        .as_ref()
                        .context("revive-dev-node requires an eth-rpc configuration")?;
                    let node = ReviveDevNode::new(
                        working_directory_configuration,
                        eth_rpc_configuration,
                        wallet_configuration,
                        &revive_dev_node_configuration,
                    )
                    .context("Failed to spawn revive-dev-node")?;
                    NodeConnector::new(node, wallet, node_configurations).await
                }
                AnyNodeConfiguration::PolkadotOmnichainNode(
                    polkadot_omnichain_node_configuration,
                ) => {
                    let eth_rpc_configuration = eth_rpc_configuration
                        .as_ref()
                        .context("polkadot-omni-node requires an eth-rpc configuration")?;
                    let node = PolkadotOmnichainNode::new(
                        working_directory_configuration,
                        eth_rpc_configuration,
                        wallet_configuration,
                        &polkadot_omnichain_node_configuration,
                    )
                    .context("Failed to spawn polkadot-omni-node")?;
                    NodeConnector::new(node, wallet, node_configurations).await
                }
            }
        })
    }

    fn new_compiler(
        &self,
        _: &CargoConfiguration,
        _: &SolcConfiguration,
        _: &ResolcConfiguration,
        working_directory_configuration: &WorkingDirectoryConfiguration,
        version: Option<VersionOrRequirement>,
    ) -> StaticFuture<Result<Box<dyn ContractCompiler + Send + Sync>>> {
        match &self.descriptor.compiler {
            AnyCompilerConfiguration::Cargo(configuration) => new_cargo_compiler(
                configuration.clone(),
                working_directory_configuration.clone(),
            ),
            AnyCompilerConfiguration::Solc(configuration) => new_solc_compiler(
                configuration.clone(),
                working_directory_configuration.clone(),
                version,
            ),
            AnyCompilerConfiguration::Resolc { solc, resolc } => new_resolc_compiler(
                solc.clone(),
                resolc.clone(),
                working_directory_configuration.clone(),
                version,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct GethEvmSolcPlatform;

impl Platform for GethEvmSolcPlatform {
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> =
            LazyLock::new(|| PlatformName::new(PlatformIdentifier::GethEvmSolc.to_string()));
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::LighthouseGethEvmSolc.to_string())
        });
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::ReviveDevNodePolkavmResolc.to_string())
        });
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::ReviveDevNodeRevmSolc.to_string())
        });
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::ZombienetPolkavmResolc.to_string())
        });
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> =
            LazyLock::new(|| PlatformName::new(PlatformIdentifier::ZombienetRevmSolc.to_string()));
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::PolkadotOmniNodePolkavmResolc.to_string())
        });
        &PLATFORM_NAME
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
    fn platform_name(&self) -> &PlatformName {
        static PLATFORM_NAME: LazyLock<PlatformName> = LazyLock::new(|| {
            PlatformName::new(PlatformIdentifier::PolkadotOmniNodeRevmSolc.to_string())
        });
        &PLATFORM_NAME
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

fn new_geth_node(context: Context) -> StaticFuture<Result<NodeConnector>> {
    Box::pin(async move {
        let (
            working_directory_configuration,
            wallet_configuration,
            geth_configuration,
            subscription_kind,
        ) = match &context {
            Context::Test(context) => (
                &context.working_directory,
                &context.wallet,
                &context.geth,
                BlockProvisioningSubscriptionKind::BestBlocks,
            ),
            Context::Benchmark(context) => (
                &context.working_directory,
                &context.wallet,
                &context.geth,
                BlockProvisioningSubscriptionKind::FinalizedBlocks,
            ),
            Context::ExportJsonSchema(_)
            | Context::ExportTestSpecifiers(_)
            | Context::Compile(_) => {
                anyhow::bail!("Nodes can only be created for tests and benchmarks")
            }
        };
        let wallet = wallet_configuration.wallet();
        let node_configurations = node_configurations(
            subscription_kind,
            geth_configuration.connector_configurations.as_deref(),
        )
        .context("Failed to parse --geth.connector-configurations as a JSON node connector configuration")?;
        let node = GethNode::new(
            working_directory_configuration,
            wallet_configuration,
            geth_configuration,
        )
        .context("Failed to spawn geth node")?;
        NodeConnector::new(node, wallet, node_configurations).await
    })
}

fn new_lighthouse_geth_node(context: Context) -> StaticFuture<Result<NodeConnector>> {
    Box::pin(async move {
        let (
            working_directory_configuration,
            wallet_configuration,
            kurtosis_configuration,
            subscription_kind,
        ) = match &context {
            Context::Test(context) => (
                &context.working_directory,
                &context.wallet,
                &context.kurtosis,
                BlockProvisioningSubscriptionKind::BestBlocks,
            ),
            Context::Benchmark(context) => (
                &context.working_directory,
                &context.wallet,
                &context.kurtosis,
                BlockProvisioningSubscriptionKind::FinalizedBlocks,
            ),
            Context::ExportJsonSchema(_)
            | Context::ExportTestSpecifiers(_)
            | Context::Compile(_) => {
                anyhow::bail!("Nodes can only be created for tests and benchmarks")
            }
        };
        let wallet = wallet_configuration.wallet();
        let node_configurations = node_configurations(
            subscription_kind,
            kurtosis_configuration.connector_configurations.as_deref(),
        )
        .context("Failed to parse --kurtosis.connector-configurations as a JSON node connector configuration")?;
        let node = LighthouseGethNode::new(
            working_directory_configuration,
            wallet_configuration,
            kurtosis_configuration,
        )
        .context("Failed to spawn lighthouse node")?;
        NodeConnector::new(node, wallet, node_configurations).await
    })
}

fn new_revive_dev_node(context: Context) -> StaticFuture<Result<NodeConnector>> {
    Box::pin(async move {
        let (
            working_directory_configuration,
            eth_rpc_configuration,
            wallet_configuration,
            revive_dev_node_configuration,
            subscription_kind,
        ) = match &context {
            Context::Test(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.revive_dev_node,
                BlockProvisioningSubscriptionKind::BestBlocks,
            ),
            Context::Benchmark(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.revive_dev_node,
                BlockProvisioningSubscriptionKind::FinalizedBlocks,
            ),
            Context::ExportJsonSchema(_)
            | Context::ExportTestSpecifiers(_)
            | Context::Compile(_) => {
                anyhow::bail!("Nodes can only be created for tests and benchmarks")
            }
        };
        let wallet = wallet_configuration.wallet();
        let node_configurations = node_configurations(
            subscription_kind,
            revive_dev_node_configuration
                .connector_configurations
                .as_deref(),
        )
        .context("Failed to parse --revive-dev-node.connector-configurations as a JSON node connector configuration")?;
        let node = ReviveDevNode::new(
            working_directory_configuration,
            eth_rpc_configuration,
            wallet_configuration,
            revive_dev_node_configuration,
        )
        .context("Failed to spawn revive-dev-node")?;
        NodeConnector::new(node, wallet, node_configurations).await
    })
}

#[cfg(unix)]
fn new_zombienet_node(context: Context) -> StaticFuture<Result<NodeConnector>> {
    Box::pin(async move {
        let (
            working_directory_configuration,
            eth_rpc_configuration,
            wallet_configuration,
            zombienet_configuration,
            subscription_kind,
        ) = match &context {
            Context::Test(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.zombienet,
                BlockProvisioningSubscriptionKind::BestBlocks,
            ),
            Context::Benchmark(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.zombienet,
                BlockProvisioningSubscriptionKind::FinalizedBlocks,
            ),
            Context::ExportJsonSchema(_)
            | Context::ExportTestSpecifiers(_)
            | Context::Compile(_) => {
                anyhow::bail!("Nodes can only be created for tests and benchmarks")
            }
        };
        let wallet = wallet_configuration.wallet();
        let node_configurations = node_configurations(
            subscription_kind,
            zombienet_configuration.connector_configurations.as_deref(),
        )
        .context("Failed to parse --zombienet.connector-configurations as a JSON node connector configuration")?;
        let node = tokio::task::block_in_place(|| {
            ZombienetNode::new(
                working_directory_configuration,
                eth_rpc_configuration,
                wallet_configuration,
                zombienet_configuration,
            )
        })
        .context("Failed to spawn zombienet")?;
        NodeConnector::new(node, wallet, node_configurations).await
    })
}

fn new_polkadot_omni_node(context: Context) -> StaticFuture<Result<NodeConnector>> {
    Box::pin(async move {
        let (
            working_directory_configuration,
            eth_rpc_configuration,
            wallet_configuration,
            polkadot_omnichain_node_configuration,
            subscription_kind,
        ) = match &context {
            Context::Test(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.polkadot_omnichain_node,
                BlockProvisioningSubscriptionKind::BestBlocks,
            ),
            Context::Benchmark(context) => (
                &context.working_directory,
                &context.eth_rpc,
                &context.wallet,
                &context.polkadot_omnichain_node,
                BlockProvisioningSubscriptionKind::FinalizedBlocks,
            ),
            Context::ExportJsonSchema(_)
            | Context::ExportTestSpecifiers(_)
            | Context::Compile(_) => {
                anyhow::bail!("Nodes can only be created for tests and benchmarks")
            }
        };
        let wallet = wallet_configuration.wallet();
        let node_configurations = node_configurations(
            subscription_kind,
            polkadot_omnichain_node_configuration
                .connector_configurations
                .as_deref(),
        )
        .context("Failed to parse --polkadot-omni-node.connector-configurations as a JSON node connector configuration")?;
        let node = PolkadotOmnichainNode::new(
            working_directory_configuration,
            eth_rpc_configuration,
            wallet_configuration,
            polkadot_omnichain_node_configuration,
        )
        .context("Failed to spawn polkadot-omni-node")?;
        NodeConnector::new(node, wallet, node_configurations).await
    })
}

fn node_configurations(
    subscription_kind: BlockProvisioningSubscriptionKind,
    user_config: Option<&str>,
) -> Result<impl Iterator<Item = NodeConnectorConfiguration> + use<>> {
    let user_config = user_config
        .map(serde_json::from_str::<NodeConnectorConfiguration>)
        .transpose()?;
    let core_config = NodeConnectorConfiguration {
        block_provisioning_behavior: Some(BlockProvisioningBehavior {
            subscription_kind: Some(subscription_kind),
        }),
        ..Default::default()
    };

    Ok(user_config.into_iter().chain(std::iter::once(core_config)))
}

fn new_solc_compiler(
    solc_configuration: SolcConfiguration,
    working_directory_configuration: WorkingDirectoryConfiguration,
    version: Option<VersionOrRequirement>,
) -> StaticFuture<Result<Box<dyn ContractCompiler + Send + Sync>>> {
    Box::pin(async move {
        let compiler =
            Solc::new_native(solc_configuration, working_directory_configuration, version).await;
        compiler.map(|compiler| Box::new(compiler) as _)
    })
}

fn new_resolc_compiler(
    solc_configuration: SolcConfiguration,
    resolc_configuration: ResolcConfiguration,
    working_directory_configuration: WorkingDirectoryConfiguration,
    version: Option<VersionOrRequirement>,
) -> StaticFuture<Result<Box<dyn ContractCompiler + Send + Sync>>> {
    Box::pin(async move {
        let compiler = Resolc::new(
            solc_configuration,
            resolc_configuration,
            working_directory_configuration,
            version,
        )
        .await;
        compiler.map(|compiler| Box::new(compiler) as _)
    })
}

/// Creates a Cargo compiler using the configured Rust toolchain.
fn new_cargo_compiler(
    cargo_configuration: CargoConfiguration,
    working_directory_configuration: WorkingDirectoryConfiguration,
) -> StaticFuture<Result<Box<dyn ContractCompiler + Send + Sync>>> {
    Box::pin(async move {
        let compiler =
            CargoCompiler::new(cargo_configuration, working_directory_configuration).await;
        compiler.map(|compiler| Box::new(compiler) as _)
    })
}
