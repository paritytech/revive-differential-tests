use std::str::FromStr;

use revive_common::EVMVersion;

use anyhow::{Error, Result, bail};

/// The configuration parameters provided in the solidity semantic tests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TestConfiguration {
    /// Controls if the test case compiles through the Yul IR.
    pub compile_via_yul: Option<ItemConfig>,
    /// Controls if the compilation should be done to EWASM.
    pub compile_to_ewasm: Option<ItemConfig>,
    /// Controls if ABI encoding should be restricted to the V1 ABI encoder.
    pub abi_encoder_v1_only: Option<ItemConfig>,
    /// Controls the EVM Version that the test is compatible with.
    pub evm_version: Option<EvmVersionRequirement>,
    /// Controls how the revert strings should be handled.
    pub revert_strings: Option<RevertString>,
    /// Controls if non-existent functions should be permitted or not.
    pub allow_non_existing_functions: Option<bool>,
    /// The list of bytecode formats that this test should be run against.
    pub bytecode_format: Option<Vec<BytecodeFormat>>,
}

impl TestConfiguration {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(
        &mut self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> Result<&mut Self> {
        match key.as_ref() {
            "compileViaYul" => self.compile_via_yul = Some(value.as_ref().parse()?),
            "compileToEwasm" => self.compile_to_ewasm = Some(value.as_ref().parse()?),
            "ABIEncoderV1Only" => self.abi_encoder_v1_only = Some(value.as_ref().parse()?),
            "EVMVersion" => self.evm_version = Some(value.as_ref().parse()?),
            "revertStrings" => self.revert_strings = Some(value.as_ref().parse()?),
            "allowNonExistingFunctions" => {
                self.allow_non_existing_functions = Some(value.as_ref().parse()?)
            }
            "bytecodeFormat" => {
                self.bytecode_format = Some(
                    value
                        .as_ref()
                        .split(',')
                        .map(str::trim)
                        .map(FromStr::from_str)
                        .collect::<Result<Vec<_>>>()?,
                )
            }
            _ => bail!("Unknown test configuration {}", key.as_ref()),
        };
        Ok(self)
    }

    pub fn new_from_pairs(
        pairs: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>,
    ) -> Result<Self> {
        let mut this = Self::default();
        pairs
            .into_iter()
            .try_fold(&mut this, |this, (key, value)| this.with_config(key, value))?;
        Ok(this)
    }
}

/// The configuration of a single item in the test configuration.
#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemConfig {
    /// The configuration is set to e a boolean that's either `true` or `false`.
    Boolean(bool),
    /// The `also`
    Also,
}

impl FromStr for ItemConfig {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "true" => Ok(Self::Boolean(true)),
            "false" => Ok(Self::Boolean(false)),
            "also" => Ok(Self::Also),
            _ => bail!("Invalid ItemConfig {s}"),
        }
    }
}

impl From<bool> for ItemConfig {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl TryFrom<String> for ItemConfig {
    type Error = <ItemConfig as FromStr>::Err;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.as_str().parse()
    }
}

/// The options available for the revert strings.
#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum RevertString {
    #[default]
    Default,
    Debug,
    Strip,
    VerboseDebug,
}

impl FromStr for RevertString {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "default" => Ok(Self::Default),
            "debug" => Ok(Self::Debug),
            "strip" => Ok(Self::Strip),
            "verboseDebug" => Ok(Self::VerboseDebug),
            _ => bail!("Invalid RevertString {s}"),
        }
    }
}

impl TryFrom<String> for RevertString {
    type Error = <RevertString as FromStr>::Err;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.as_str().parse()
    }
}

/// The set of available bytecode formats.
#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BytecodeFormat {
    Legacy,
    EofVersionGreaterThanOne,
}

impl FromStr for BytecodeFormat {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "legacy" => Ok(Self::Legacy),
            ">=EOFv1" => Ok(Self::EofVersionGreaterThanOne),
            _ => bail!("Invalid BytecodeFormat {s}"),
        }
    }
}

impl TryFrom<String> for BytecodeFormat {
    type Error = <BytecodeFormat as FromStr>::Err;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.as_str().parse()
    }
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvmVersionRequirement {
    GreaterThan(EVMVersion),
    GreaterThanOrEqual(EVMVersion),
    LessThan(EVMVersion),
    LessThanOrEqual(EVMVersion),
    EqualTo(EVMVersion),
}

impl FromStr for EvmVersionRequirement {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.as_bytes() {
            [b'>', b'=', remaining @ ..] => Ok(Self::GreaterThanOrEqual(
                str::from_utf8(remaining)?.try_into()?,
            )),
            [b'>', remaining @ ..] => Ok(Self::GreaterThan(str::from_utf8(remaining)?.try_into()?)),
            [b'<', b'=', remaining @ ..] => Ok(Self::LessThanOrEqual(
                str::from_utf8(remaining)?.try_into()?,
            )),
            [b'<', remaining @ ..] => Ok(Self::LessThan(str::from_utf8(remaining)?.try_into()?)),
            [b'=', remaining @ ..] => Ok(Self::EqualTo(str::from_utf8(remaining)?.try_into()?)),
            _ => bail!("Invalid EVM version requirement {s}"),
        }
    }
}

impl TryFrom<String> for EvmVersionRequirement {
    type Error = <EvmVersionRequirement as FromStr>::Err;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.as_str().parse()
    }
}
