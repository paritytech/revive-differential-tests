use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    fs::{File, read_to_string},
    ops::Deref,
    path::{Path, PathBuf},
    str::FromStr,
};

use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::Address;
use revive_dt_node_interaction::EthereumNode;
use serde::{Deserialize, Serialize};

use crate::{
    case::Case,
    define_wrapper_type,
    input::resolve_argument,
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
    pub fn try_from_file(path: &Path) -> Option<Self> {
        Metadata::try_from_file(path).map(|metadata| Self {
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

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Metadata {
    pub targets: Option<Vec<String>>,
    pub cases: Vec<Case>,
    pub contracts: Option<BTreeMap<ContractInstance, ContractPathAndIdentifier>>,
    // TODO: Convert into wrapper types for clarity.
    pub libraries: Option<BTreeMap<String, BTreeMap<String, String>>>,
    pub ignore: Option<bool>,
    pub modes: Option<Vec<Mode>>,
    pub file_path: Option<PathBuf>,
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
    ) -> anyhow::Result<BTreeMap<ContractInstance, ContractPathAndIdentifier>> {
        let directory = self.directory()?;
        let mut sources = BTreeMap::new();
        let Some(contracts) = &self.contracts else {
            return Ok(sources);
        };

        for (
            alias,
            ContractPathAndIdentifier {
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
                ContractPathAndIdentifier {
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
    pub fn try_from_file(path: &Path) -> Option<Self> {
        assert!(path.is_file(), "not a file: {}", path.display());

        let Some(file_extension) = path.extension() else {
            tracing::debug!("skipping corpus file: {}", path.display());
            return None;
        };

        if file_extension == METADATA_FILE_EXTENSION {
            return Self::try_from_json(path);
        }

        if file_extension == SOLIDITY_CASE_FILE_EXTENSION {
            return Self::try_from_solidity(path);
        }

        tracing::debug!("ignoring invalid corpus file: {}", path.display());
        None
    }

    fn try_from_json(path: &Path) -> Option<Self> {
        let file = File::open(path)
            .inspect_err(|error| {
                tracing::error!(
                    "opening JSON test metadata file '{}' error: {error}",
                    path.display()
                );
            })
            .ok()?;

        match serde_json::from_reader::<_, Metadata>(file) {
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

    fn try_from_solidity(path: &Path) -> Option<Self> {
        let spec = read_to_string(path)
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
                        ContractInstance::new_from("test"),
                        ContractPathAndIdentifier {
                            contract_source_path: path.to_path_buf(),
                            contract_ident: ContractIdent::new_from("Test"),
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

    pub fn handle_address_replacement(
        &mut self,
        old_to_new_mapping: &mut AddressReplacementMap,
    ) -> anyhow::Result<()> {
        for case in self.cases.iter_mut() {
            case.handle_address_replacement(old_to_new_mapping)?;
        }
        tracing::debug!(metadata = ?self, "Performed replacement on metadata");
        Ok(())
    }
}

define_wrapper_type!(
    /// Represents a contract instance found a metadata file.
    ///
    /// Typically, this is used as the key to the "contracts" field of metadata files.
    #[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    ContractInstance(String);
);

define_wrapper_type!(
    /// Represents a contract identifier found a metadata file.
    ///
    /// A contract identifier is the name of the contract in the source code.
    #[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    ContractIdent(String);
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
pub struct ContractPathAndIdentifier {
    /// The path of the contract source code relative to the directory containing the metadata file.
    pub contract_source_path: PathBuf,

    /// The identifier of the contract.
    pub contract_ident: ContractIdent,
}

impl Display for ContractPathAndIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.contract_source_path.display(),
            self.contract_ident.as_ref()
        )
    }
}

impl FromStr for ContractPathAndIdentifier {
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
        let Some(path) = path else {
            anyhow::bail!("Path is not defined");
        };
        let Some(identifier) = identifier else {
            anyhow::bail!("Contract identifier is not defined")
        };
        Ok(Self {
            contract_source_path: PathBuf::from(path),
            contract_ident: ContractIdent::new(identifier),
        })
    }
}

impl TryFrom<String> for ContractPathAndIdentifier {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<ContractPathAndIdentifier> for String {
    fn from(value: ContractPathAndIdentifier) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub struct AddressReplacementMap(HashMap<Address, (PrivateKeySigner, Address)>);

impl AddressReplacementMap {
    pub fn new() -> Self {
        Self(Default::default())
    }

    pub fn into_inner(self) -> HashMap<Address, (PrivateKeySigner, Address)> {
        self.0
    }

    pub fn contains_key(&self, address: &Address) -> bool {
        self.0.contains_key(address)
    }

    pub fn add(&mut self, address: Address) -> Address {
        self.0
            .entry(address)
            .or_insert_with(|| {
                let private_key = Self::new_random_private_key_signer();
                let account = private_key.address();
                tracing::debug!(
                    old_address = %address,
                    new_address = %account,
                    "Added a new address replacement"
                );
                (private_key, account)
            })
            .1
    }

    pub fn resolve(&self, value: &str) -> Option<Address> {
        // We attempt to resolve the given string without any additional context of the deployed
        // contracts or the node API as we do not need them. If the resolution fails then we know
        // that this isn't an address and we skip it.
        let Ok(resolved) = resolve_argument(value, &Default::default(), &UnimplementedEthereumNode)
        else {
            return None;
        };
        let resolved_bytes = resolved.to_be_bytes_trimmed_vec();
        let Ok(address) = Address::try_from(resolved_bytes.as_slice()) else {
            return None;
        };
        self.0.get(&address).map(|(_, address)| *address)
    }

    fn new_random_private_key_signer() -> PrivateKeySigner {
        // TODO: Use a seedable RNG to allow for deterministic allocation of the private keys so
        // that we get reproducible runs.
        PrivateKeySigner::random()
    }
}

impl AsRef<HashMap<Address, (PrivateKeySigner, Address)>> for AddressReplacementMap {
    fn as_ref(&self) -> &HashMap<Address, (PrivateKeySigner, Address)> {
        &self.0
    }
}

struct UnimplementedEthereumNode;

impl EthereumNode for UnimplementedEthereumNode {
    fn execute_transaction(
        &self,
        _: alloy::rpc::types::TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        anyhow::bail!("Unimplemented")
    }

    fn chain_id(&self) -> anyhow::Result<alloy_primitives::ChainId> {
        anyhow::bail!("Unimplemented")
    }

    fn block_gas_limit(&self, _: alloy::eips::BlockNumberOrTag) -> anyhow::Result<u128> {
        anyhow::bail!("Unimplemented")
    }

    fn block_coinbase(&self, _: alloy::eips::BlockNumberOrTag) -> anyhow::Result<Address> {
        anyhow::bail!("Unimplemented")
    }

    fn block_difficulty(
        &self,
        _: alloy::eips::BlockNumberOrTag,
    ) -> anyhow::Result<alloy_primitives::U256> {
        anyhow::bail!("Unimplemented")
    }

    fn block_hash(
        &self,
        _: alloy::eips::BlockNumberOrTag,
    ) -> anyhow::Result<alloy_primitives::BlockHash> {
        anyhow::bail!("Unimplemented")
    }

    fn block_timestamp(
        &self,
        _: alloy::eips::BlockNumberOrTag,
    ) -> anyhow::Result<alloy_primitives::BlockTimestamp> {
        anyhow::bail!("Unimplemented")
    }

    fn last_block_number(&self) -> anyhow::Result<alloy_primitives::BlockNumber> {
        anyhow::bail!("Unimplemented")
    }

    fn trace_transaction(
        &self,
        _: &alloy::rpc::types::TransactionReceipt,
        _: alloy::rpc::types::trace::geth::GethDebugTracingOptions,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        anyhow::bail!("Unimplemented")
    }

    fn state_diff(
        &self,
        _: &alloy::rpc::types::TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::DiffMode> {
        anyhow::bail!("Unimplemented")
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn contract_identifier_respects_roundtrip_property() {
        // Arrange
        let string = "ERC20/ERC20.sol:ERC20";

        // Act
        let identifier = ContractPathAndIdentifier::from_str(string);

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
