use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

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
)]
#[serde(rename = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum PlatformIdentifier {
    /// The Go-ethereum reference full node EVM implementation.
    GethEvm,
    /// The kitchensink node with the PolkaVM backend.
    KitchensinkPolkaVM,
    /// The kitchensink node with the REVM backend.
    KitchensinkREVM,
    /// The revive dev node with the PolkaVM backend.
    ReviveDevNodePolkaVM,
    /// The revive dev node with the REVM backend.
    ReviveDevNodeREVM,
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
)]
#[serde(rename = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
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
)]
#[serde(rename = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum NodeIdentifier {
    /// The go-ethereum node implementation.
    Geth,
    /// The Kitchensink node implementation.
    Kitchensink,
    /// The revive dev node implementation.
    ReviveDevNode,
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
)]
#[serde(rename = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum VmIdentifier {
    /// The ethereum virtual machine.
    Evm,
    /// Polkadot's PolaVM Risc-v based virtual machine.
    PolkaVm,
}
