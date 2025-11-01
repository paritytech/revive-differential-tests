use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

/// An enum of the platform identifiers of all of the platforms supported by this framework. This
/// could be thought of like the target triple from Rust and LLVM where it specifies the platform
/// completely starting with the node, the vm, and finally the compiler used for this combination.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum PlatformIdentifier {
    /// The Go-ethereum reference full node EVM implementation with the solc compiler.
    GethEvmSolc,
    /// The Lighthouse Go-ethereum reference full node EVM implementation with the solc compiler.
    LighthouseGethEvmSolc,
    /// The revive dev node with the PolkaVM backend with the resolc compiler.
    ReviveDevNodePolkavmResolc,
    /// The revive dev node with the REVM backend with the solc compiler.
    ReviveDevNodeRevmSolc,
    /// A zombienet based Substrate/Polkadot node with the PolkaVM backend with the resolc compiler.
    ZombienetPolkavmResolc,
    /// A zombienet based Substrate/Polkadot node with the REVM backend with the solc compiler.
    ZombienetRevmSolc,
}

/// An enum of the platform identifiers of all of the platforms supported by this framework.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
    JsonSchema,
)]
pub enum CompilerIdentifier {
    /// The solc compiler.
    Solc,
    /// The resolc compiler.
    Resolc,
}

/// An enum representing the identifiers of the supported nodes.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
    JsonSchema,
)]
pub enum NodeIdentifier {
    /// The go-ethereum node implementation.
    Geth,
    /// The go-ethereum node implementation.
    LighthouseGeth,
    /// The revive dev node implementation.
    ReviveDevNode,
    /// A zombienet spawned nodes
    Zombienet,
}

/// An enum representing the identifiers of the supported VMs.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
    Display,
    AsRefStr,
    IntoStaticStr,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum VmIdentifier {
    /// The ethereum virtual machine.
    Evm,
    /// The EraVM virtual machine.
    EraVM,
    /// Polkadot's PolaVM Risc-v based virtual machine.
    PolkaVM,
}
