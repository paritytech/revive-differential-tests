use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fmt::Display,
    ops::Deref,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use revive_common::EVMVersion;
use revive_dt_common::{
    fs::CachedFileSystem, iterators::FilesWithExtensionIterator, macros::define_wrapper_type,
};

use crate::{
    case::Case,
    mode::{Mode, SolcMode},
};

pub const METADATA_FILE_EXTENSION: &str = "json";
pub const SOLIDITY_CASE_FILE_EXTENSION: &str = "sol";
pub const SOLIDITY_CASE_COMMENT_MARKER: &str = "//!";

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct MetadataFile {
    pub path: PathBuf,
    pub content: Metadata,
}

impl MetadataFile {
    pub async fn try_from_file(path: &Path) -> Option<Self> {
        Metadata::try_from_file(path).await.map(|metadata| Self {
            path: path.to_owned(),
            content: metadata,
        })
    }
}

impl Deref for MetadataFile {
    type Target = Metadata;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct Metadata {
    /// A comment on the test case that's added for human-readability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,

    pub cases: Vec<Case>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contracts: Option<BTreeMap<ContractInstance, ContractPathAndIdent>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub libraries: Option<BTreeMap<PathBuf, BTreeMap<ContractIdent, ContractInstance>>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<Mode>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<PathBuf>,

    /// This field specifies an EVM version requirement that the test case has where the test might
    /// be run of the evm version of the nodes match the evm version specified here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_evm_version: Option<EvmVersionRequirement>,

    /// A set of compilation directives that will be passed to the compiler whenever the contracts for
    /// the test are being compiled. Note that this differs from the [`Mode`]s in that a [`Mode`] is
    /// just a filter for when a test can run whereas this is an instruction to the compiler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_directives: Option<CompilationDirectives>,
}

impl Metadata {
    /// Returns the solc modes of this metadata, inserting a default mode if not present.
    pub fn solc_modes(&self) -> Vec<SolcMode> {
        self.modes
            .to_owned()
            .unwrap_or_else(|| vec![Mode::Solidity(Default::default())])
            .iter()
            .filter_map(|mode| match mode {
                Mode::Solidity(solc_mode) => Some(solc_mode),
                Mode::Unknown(mode) => {
                    tracing::debug!("compiler: ignoring unknown mode '{mode}'");
                    None
                }
            })
            .cloned()
            .collect()
    }

    /// Returns the base directory of this metadata.
    pub fn directory(&self) -> anyhow::Result<PathBuf> {
        Ok(self
            .file_path
            .as_ref()
            .and_then(|path| path.parent())
            .ok_or_else(|| anyhow::anyhow!("metadata invalid file path: {:?}", self.file_path))?
            .to_path_buf())
    }

    /// Returns the contract sources with canonicalized paths for the files
    pub fn contract_sources(
        &self,
    ) -> anyhow::Result<BTreeMap<ContractInstance, ContractPathAndIdent>> {
        let directory = self.directory()?;
        let mut sources = BTreeMap::new();
        let Some(contracts) = &self.contracts else {
            return Ok(sources);
        };

        for (
            alias,
            ContractPathAndIdent {
                contract_source_path,
                contract_ident,
            },
        ) in contracts
        {
            let alias = alias.clone();
            let absolute_path = directory.join(contract_source_path).canonicalize()?;
            let contract_ident = contract_ident.clone();

            sources.insert(
                alias,
                ContractPathAndIdent {
                    contract_source_path: absolute_path,
                    contract_ident,
                },
            );
        }

        Ok(sources)
    }

    /// Try to parse the test metadata struct from the given file at `path`.
    ///
    /// Returns `None` if `path` didn't contain a test metadata or case definition.
    ///
    /// # Panics
    /// Expects the supplied `path` to be a file.
    pub async fn try_from_file(path: &Path) -> Option<Self> {
        assert!(path.is_file(), "not a file: {}", path.display());

        let Some(file_extension) = path.extension() else {
            tracing::debug!("skipping corpus file: {}", path.display());
            return None;
        };

        if file_extension == METADATA_FILE_EXTENSION {
            return Self::try_from_json(path).await;
        }

        if file_extension == SOLIDITY_CASE_FILE_EXTENSION {
            return Self::try_from_solidity(path).await;
        }

        tracing::debug!("ignoring invalid corpus file: {}", path.display());
        None
    }

    async fn try_from_json(path: &Path) -> Option<Self> {
        let content = CachedFileSystem::read(path)
            .await
            .inspect_err(|error| {
                tracing::error!(
                    "opening JSON test metadata file '{}' error: {error}",
                    path.display()
                );
            })
            .ok()?;

        match serde_json::from_slice::<Metadata>(content.as_slice()) {
            Ok(mut metadata) => {
                metadata.file_path = Some(path.to_path_buf());
                Some(metadata)
            }
            Err(error) => {
                tracing::error!(
                    "parsing JSON test metadata file '{}' error: {error}",
                    path.display()
                );
                None
            }
        }
    }

    async fn try_from_solidity(path: &Path) -> Option<Self> {
        let spec = CachedFileSystem::read_to_string(path)
            .await
            .inspect_err(|error| {
                tracing::error!(
                    "opening JSON test metadata file '{}' error: {error}",
                    path.display()
                );
            })
            .ok()?
            .lines()
            .filter_map(|line| line.strip_prefix(SOLIDITY_CASE_COMMENT_MARKER))
            .fold(String::new(), |mut buf, string| {
                buf.push_str(string);
                buf
            });

        if spec.is_empty() {
            return None;
        }

        match serde_json::from_str::<Self>(&spec) {
            Ok(mut metadata) => {
                metadata.file_path = Some(path.to_path_buf());
                metadata.contracts = Some(
                    [(
                        ContractInstance::new("Test"),
                        ContractPathAndIdent {
                            contract_source_path: path.to_path_buf(),
                            contract_ident: ContractIdent::new("Test"),
                        },
                    )]
                    .into(),
                );
                Some(metadata)
            }
            Err(error) => {
                tracing::error!(
                    "parsing Solidity test metadata file '{}' error: '{error}' from data: {spec}",
                    path.display()
                );
                None
            }
        }
    }

    /// Returns an iterator over all of the solidity files that needs to be compiled for this
    /// [`Metadata`] object
    ///
    /// Note: if the metadata is contained within a solidity file then this is the only file that
    /// we wish to compile since this is a self-contained test. Otherwise, if it's a JSON file
    /// then we need to compile all of the contracts that are in the directory since imports are
    /// allowed in there.
    pub fn files_to_compile(&self) -> anyhow::Result<Box<dyn Iterator<Item = PathBuf>>> {
        let Some(ref metadata_file_path) = self.file_path else {
            anyhow::bail!("The metadata file path is not defined");
        };
        if metadata_file_path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sol"))
        {
            Ok(Box::new(std::iter::once(metadata_file_path.clone())))
        } else {
            Ok(Box::new(
                FilesWithExtensionIterator::new(self.directory()?).with_allowed_extension("sol"),
            ))
        }
    }
}

define_wrapper_type!(
    /// Represents a contract instance found a metadata file.
    ///
    /// Typically, this is used as the key to the "contracts" field of metadata files.
    #[derive(
        Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
    )]
    #[serde(transparent)]
    pub struct ContractInstance(String);
);

define_wrapper_type!(
    /// Represents a contract identifier found a metadata file.
    ///
    /// A contract identifier is the name of the contract in the source code.
    #[derive(
        Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
    )]
    #[serde(transparent)]
    pub struct ContractIdent(String);
);

/// Represents an identifier used for contracts.
///
/// The type supports serialization from and into the following string format:
///
/// ```text
/// ${path}:${contract_ident}
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContractPathAndIdent {
    /// The path of the contract source code relative to the directory containing the metadata file.
    pub contract_source_path: PathBuf,

    /// The identifier of the contract.
    pub contract_ident: ContractIdent,
}

impl Display for ContractPathAndIdent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.contract_source_path.display(),
            self.contract_ident.as_ref()
        )
    }
}

impl FromStr for ContractPathAndIdent {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut splitted_string = s.split(":").peekable();
        let mut path = None::<String>;
        let mut identifier = None::<String>;
        loop {
            let Some(next_item) = splitted_string.next() else {
                break;
            };
            if splitted_string.peek().is_some() {
                match path {
                    Some(ref mut path) => {
                        path.push(':');
                        path.push_str(next_item);
                    }
                    None => path = Some(next_item.to_owned()),
                }
            } else {
                identifier = Some(next_item.to_owned())
            }
        }
        match (path, identifier) {
            (Some(path), Some(identifier)) => Ok(Self {
                contract_source_path: PathBuf::from(path),
                contract_ident: ContractIdent::new(identifier),
            }),
            (None, Some(path)) | (Some(path), None) => {
                let Some(identifier) = path.split(".").next().map(ToOwned::to_owned) else {
                    anyhow::bail!("Failed to find identifier");
                };
                Ok(Self {
                    contract_source_path: PathBuf::from(path),
                    contract_ident: ContractIdent::new(identifier),
                })
            }
            (None, None) => anyhow::bail!("Failed to find the path and identifier"),
        }
    }
}

impl TryFrom<String> for ContractPathAndIdent {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<ContractPathAndIdent> for String {
    fn from(value: ContractPathAndIdent) -> Self {
        value.to_string()
    }
}

/// An EVM version requirement that the test case has. This gets serialized and
/// deserialized from and into [`String`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EvmVersionRequirement {
    ordering: Ordering,
    or_equal: bool,
    evm_version: EVMVersion,
}

impl EvmVersionRequirement {
    pub fn new_greater_than_or_equals(version: EVMVersion) -> Self {
        Self {
            ordering: Ordering::Greater,
            or_equal: true,
            evm_version: version,
        }
    }

    pub fn new_greater_than(version: EVMVersion) -> Self {
        Self {
            ordering: Ordering::Greater,
            or_equal: false,
            evm_version: version,
        }
    }

    pub fn new_equals(version: EVMVersion) -> Self {
        Self {
            ordering: Ordering::Equal,
            or_equal: false,
            evm_version: version,
        }
    }

    pub fn new_less_than(version: EVMVersion) -> Self {
        Self {
            ordering: Ordering::Less,
            or_equal: false,
            evm_version: version,
        }
    }

    pub fn new_less_than_or_equals(version: EVMVersion) -> Self {
        Self {
            ordering: Ordering::Less,
            or_equal: true,
            evm_version: version,
        }
    }

    pub fn matches(&self, other: &EVMVersion) -> bool {
        let ordering = other.cmp(&self.evm_version);
        ordering == self.ordering || (self.or_equal && matches!(ordering, Ordering::Equal))
    }
}

impl Display for EvmVersionRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            ordering,
            or_equal,
            evm_version,
        } = self;
        match ordering {
            Ordering::Less => write!(f, "<")?,
            Ordering::Equal => write!(f, "=")?,
            Ordering::Greater => write!(f, ">")?,
        }
        if *or_equal && !matches!(ordering, Ordering::Equal) {
            write!(f, "=")?;
        }
        write!(f, "{evm_version}")
    }
}

impl FromStr for EvmVersionRequirement {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.as_bytes() {
            [b'>', b'=', remaining @ ..] => Ok(Self {
                ordering: Ordering::Greater,
                or_equal: true,
                evm_version: str::from_utf8(remaining)?.try_into()?,
            }),
            [b'>', remaining @ ..] => Ok(Self {
                ordering: Ordering::Greater,
                or_equal: false,
                evm_version: str::from_utf8(remaining)?.try_into()?,
            }),
            [b'<', b'=', remaining @ ..] => Ok(Self {
                ordering: Ordering::Less,
                or_equal: true,
                evm_version: str::from_utf8(remaining)?.try_into()?,
            }),
            [b'<', remaining @ ..] => Ok(Self {
                ordering: Ordering::Less,
                or_equal: false,
                evm_version: str::from_utf8(remaining)?.try_into()?,
            }),
            [b'=', remaining @ ..] => Ok(Self {
                ordering: Ordering::Equal,
                or_equal: false,
                evm_version: str::from_utf8(remaining)?.try_into()?,
            }),
            _ => anyhow::bail!("Invalid EVM version requirement {s}"),
        }
    }
}

impl TryFrom<String> for EvmVersionRequirement {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<EvmVersionRequirement> for String {
    fn from(value: EvmVersionRequirement) -> Self {
        value.to_string()
    }
}

/// A set of compilation directives that will be passed to the compiler whenever the contracts for
/// the test are being compiled. Note that this differs from the [`Mode`]s in that a [`Mode`] is
/// just a filter for when a test can run whereas this is an instruction to the compiler.
/// Defines how the compiler should handle revert strings.
#[derive(
    Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct CompilationDirectives {
    /// Defines how the revert strings should be handled.
    pub revert_string_handling: Option<RevertString>,
}

/// Defines how the compiler should handle revert strings.
#[derive(
    Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum RevertString {
    #[default]
    Default,
    Debug,
    Strip,
    VerboseDebug,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn contract_identifier_respects_roundtrip_property() {
        // Arrange
        let string = "ERC20/ERC20.sol:ERC20";

        // Act
        let identifier = ContractPathAndIdent::from_str(string);

        // Assert
        let identifier = identifier.expect("Failed to parse");
        assert_eq!(
            identifier.contract_source_path.display().to_string(),
            "ERC20/ERC20.sol"
        );
        assert_eq!(identifier.contract_ident, "ERC20".to_owned().into());

        // Act
        let reserialized = identifier.to_string();

        // Assert
        assert_eq!(string, reserialized);
    }

    #[test]
    fn complex_metadata_file_can_be_deserialized() {
        // Arrange
        const JSON: &str = include_str!("../../../assets/test_metadata.json");

        // Act
        let metadata = serde_json::from_str::<Metadata>(JSON);

        // Assert
        metadata.expect("Failed to deserialize metadata");
    }
}
