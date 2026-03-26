use crate::internal_prelude::*;

pub const METADATA_FILE_EXTENSION: &str = "json";
pub const SOLIDITY_CASE_FILE_EXTENSION: &str = "sol";
pub const SOLIDITY_CASE_COMMENT_MARKER: &str = "//!";

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct MetadataFile {
    /// The path of the metadata file. This will either be a JSON or solidity file.
    pub metadata_file_path: PathBuf,

    /// This is the path contained within the corpus file. This could either be the path of some dir
    /// or could be the actual metadata file path.
    pub corpus_file_path: PathBuf,

    /// The metadata contained within the file.
    pub content: Metadata,
}

impl MetadataFile {
    pub fn relative_path(&self) -> &Path {
        if self.corpus_file_path.is_file() {
            &self.corpus_file_path
        } else {
            self.metadata_file_path
                .strip_prefix(&self.corpus_file_path)
                .unwrap()
        }
    }
}

impl Deref for MetadataFile {
    type Target = Metadata;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

/// A MatterLabs metadata file.
///
/// This defines the structure that the MatterLabs metadata files follow for defining the tests or
/// the workloads.
///
/// Each metadata file is composed of multiple test cases where each test case is isolated from the
/// others and runs in a completely different address space. Each test case is composed of a number
/// of steps and assertions that should be performed as part of the test case.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema, Clone, Eq, PartialEq)]
pub struct Metadata {
    /// This is an optional comment on the metadata file which has no impact on the execution in any
    /// way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// An optional boolean which defines if the metadata file as a whole should be ignored. If null
    /// then the metadata file will not be ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<bool>,

    /// An optional vector of targets that this Metadata file's cases can be executed on. As an
    /// example, if we wish for the metadata file's cases to only be run on PolkaVM then we'd
    /// specify a target of "PolkaVM" in here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<HashSet<VmIdentifier>>,

    /// A vector of the test cases and workloads contained within the metadata file. This is their
    /// primary description.
    pub cases: Vec<Case>,

    /// A map of all of the contracts that the test requires to run.
    ///
    /// This is a map where the key is the name of the contract instance and the value is the
    /// contract's path and ident in the file.
    ///
    /// If any contract is to be used by the test then it must be included in here first so that the
    /// framework is aware of its path, compiles it, and prepares it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contracts: Option<IndexMap<ContractInstance, ContractPathAndIdent>>,

    /// The set of libraries that this metadata file requires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libraries: Option<IndexMap<PathBuf, IndexMap<ContractIdent, ContractInstance>>>,

    /// This represents a mode that has been parsed from test metadata.
    ///
    /// Mode strings can take the following form (in pseudo-regex):
    ///
    /// ```text
    /// [YEILV][+-]? (M[0123sz])? <semver>?
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<ParsedMode>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub file_path: Option<PathBuf>,

    /// This field specifies an EVM version requirement that the test case has where the test might
    /// be run of the evm version of the nodes match the evm version specified here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_evm_version: Option<EvmVersionRequirement>,

    /// A set of compilation directives that will be passed to the compiler whenever the contracts
    /// for the test are being compiled. Note that this differs from the [`Mode`]s in that a [`Mode`]
    /// is just a filter for when a test can run whereas this is an instruction to the compiler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_directives: Option<CompilationDirectives>,
}

impl Metadata {
    /// Returns the modes that we should test from this metadata.
    pub fn compiler_modes(&self) -> Vec<Mode> {
        match &self.modes {
            Some(modes) => ParsedMode::many_to_modes(modes.iter()).collect(),
            None => Mode::all().cloned().collect(),
        }
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
            let absolute_path = directory
                .join(contract_source_path)
                .canonicalize()
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to canonicalize contract source path '{}': {error}",
                        directory.join(contract_source_path).display()
                    )
                })?;
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
    pub fn try_from_file(path: &Path) -> Option<Self> {
        assert!(path.is_file(), "not a file: {}", path.display());

        let file_extension = path.extension()?;

        if file_extension == METADATA_FILE_EXTENSION {
            return Self::try_from_json(path);
        }

        if file_extension == SOLIDITY_CASE_FILE_EXTENSION {
            return Self::try_from_solidity(path);
        }

        None
    }

    fn try_from_json(path: &Path) -> Option<Self> {
        let file = File::open(path)
            .inspect_err(|err| error!(path = %path.display(), %err, "Failed to open file"))
            .ok()?;

        match serde_json::from_reader::<_, Metadata>(file) {
            Ok(mut metadata) => {
                metadata.file_path = Some(path.to_path_buf());
                Some(metadata)
            }
            Err(err) => {
                error!(path = %path.display(), %err, "Deserialization of metadata failed");
                None
            }
        }
    }

    fn try_from_solidity(path: &Path) -> Option<Self> {
        let spec = read_to_string(path)
            .inspect_err(|err| error!(path = %path.display(), %err, "Failed to read file content"))
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
            Err(err) => {
                error!(path = %path.display(), %err, "Failed to deserialize metadata");
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
                FilesWithExtensionIterator::new(self.directory()?)
                    .with_allowed_extension("sol")
                    .with_use_cached_fs(true),
            ))
        }
    }
}

define_wrapper_type!(
    /// Represents a contract instance found a metadata file.
    ///
    /// Typically, this is used as the key to the "contracts" field of metadata files.
    #[derive(
        Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema
    )]
    #[serde(transparent)]
    pub struct ContractInstance(String) impl Display;
);

define_wrapper_type!(
    /// Represents a contract identifier found a metadata file.
    ///
    /// A contract identifier is the name of the contract in the source code.
    #[derive(
        Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema
    )]
    #[serde(transparent)]
    pub struct ContractIdent(String) impl Display;
);

/// Represents an identifier used for contracts.
///
/// The type supports serialization from and into the following string format:
///
/// ```text
/// ${path}:${contract_ident}
/// ```
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
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

/// An EVM version requirement that the test case has. This gets serialized and deserialized from
/// and into [`String`]. This follows a simple format of (>=|<=|=|>|<) followed by a string of the
/// EVM version.
///
/// When specified, the framework will only run the test if the node's EVM version matches that
/// required by the metadata file.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
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
    Clone,
    Debug,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct CompilationDirectives {
    /// Defines how the revert strings should be handled.
    pub revert_string_handling: Option<RevertString>,
}

/// Defines how the compiler should handle revert strings.
#[derive(
    Clone,
    Debug,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum RevertString {
    /// The default handling of the revert strings.
    #[default]
    Default,
    /// The debug handling of the revert strings.
    Debug,
    /// Strip the revert strings.
    Strip,
    /// Provide verbose debug strings for the revert string.
    VerboseDebug,
}

define_wrapper_type!(
    /// A reference to a contract instance, resolved at runtime.
    ///
    /// Serialized and deserialized as a string with the format `$INSTANCE:{token}`, where `{token}`
    /// is either a literal index (e.g. `$INSTANCE:0`) or a variable reference
    /// (e.g. `$INSTANCE:$VARIABLE:I`).
    #[derive(
        Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
    )]
    #[serde(try_from = "String", into = "String")]
    pub struct ContractInstanceReference(CalldataToken<String>);
);

impl Display for ContractInstanceReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            CalldataToken::Item(s) => write!(f, "$INSTANCE:{s}"),
            CalldataToken::Operation(op) => write!(f, "$INSTANCE:{op:?}"),
        }
    }
}

impl FromStr for ContractInstanceReference {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let token = s.strip_prefix("$INSTANCE:").ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid contract instance reference: expected '$INSTANCE:{{token}}', got '{s}'"
            )
        })?;
        Ok(Self(CalldataToken::Item(token.to_owned())))
    }
}

impl TryFrom<String> for ContractInstanceReference {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ContractInstanceReference> for String {
    fn from(value: ContractInstanceReference) -> Self {
        value.to_string()
    }
}

/// Either a [`ContractInstanceReference`] or a [`ContractInstance`].
///
/// Either a [`ContractInstanceReference`] or a [`ContractInstance`].
///
/// Deserialized as untagged: strings matching `$INSTANCE:{index}` are parsed as a reference,
/// everything else is treated as a contract instance name.
///
/// The lifetime parameter `'a` allows the enum to borrow its inner values rather than requiring
/// owned data.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(untagged)]
pub enum ContractInstanceOrReference<'a> {
    /// A positional reference to a contract instance.
    Reference(Cow<'a, ContractInstanceReference>),
    /// A contract instance identified by name.
    Instance(Cow<'a, ContractInstance>),
}

impl From<ContractInstanceReference> for ContractInstanceOrReference<'_> {
    fn from(value: ContractInstanceReference) -> Self {
        Self::Reference(Cow::Owned(value))
    }
}

impl From<ContractInstance> for ContractInstanceOrReference<'_> {
    fn from(value: ContractInstance) -> Self {
        Self::Instance(Cow::Owned(value))
    }
}

impl<'a> From<&'a ContractInstanceReference> for ContractInstanceOrReference<'a> {
    fn from(value: &'a ContractInstanceReference) -> Self {
        Self::Reference(Cow::Borrowed(value))
    }
}

impl<'a> From<&'a ContractInstance> for ContractInstanceOrReference<'a> {
    fn from(value: &'a ContractInstance) -> Self {
        Self::Instance(Cow::Borrowed(value))
    }
}

impl<'a, 'b> From<&'a ContractInstanceOrReference<'b>> for ContractInstanceOrReference<'a> {
    fn from(value: &'a ContractInstanceOrReference<'b>) -> Self {
        match value {
            ContractInstanceOrReference::Reference(cow) => {
                Self::Reference(Cow::Borrowed(cow.as_ref()))
            }
            ContractInstanceOrReference::Instance(cow) => {
                Self::Instance(Cow::Borrowed(cow.as_ref()))
            }
        }
    }
}

impl From<String> for ContractInstanceOrReference<'_> {
    fn from(value: String) -> Self {
        match value.parse::<ContractInstanceReference>() {
            Ok(reference) => Self::Reference(Cow::Owned(reference)),
            Err(_) => Self::Instance(Cow::Owned(ContractInstance::new(value))),
        }
    }
}

impl Default for ContractInstanceOrReference<'_> {
    fn default() -> Self {
        Self::Instance(Cow::Owned(ContractInstance::default()))
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

    #[test]
    fn instance_or_reference_deserializes_reference_from_string() {
        // Arrange
        let json = r#""$INSTANCE:42""#;

        // Act
        let value: ContractInstanceOrReference = serde_json::from_str(json).unwrap();

        // Assert
        assert_eq!(
            value,
            ContractInstanceOrReference::from(ContractInstanceReference::new(CalldataToken::Item(
                "42".into()
            )))
        );
    }

    #[test]
    fn instance_or_reference_deserializes_instance_from_string() {
        // Arrange
        let json = r#""MyContract""#;

        // Act
        let value: ContractInstanceOrReference = serde_json::from_str(json).unwrap();

        // Assert
        assert_eq!(
            value,
            ContractInstanceOrReference::from(ContractInstance::new("MyContract"))
        );
    }

    #[test]
    fn instance_or_reference_roundtrips_reference() {
        // Arrange
        let original = ContractInstanceOrReference::from(ContractInstanceReference::new(
            CalldataToken::Item("7".into()),
        ));

        // Act
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ContractInstanceOrReference = serde_json::from_str(&json).unwrap();

        // Assert
        assert_eq!(json, r#""$INSTANCE:7""#);
        assert_eq!(deserialized, original);
    }

    #[test]
    fn instance_or_reference_roundtrips_instance() {
        // Arrange
        let original = ContractInstanceOrReference::from(ContractInstance::new("Token"));

        // Act
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ContractInstanceOrReference = serde_json::from_str(&json).unwrap();

        // Assert
        assert_eq!(json, r#""Token""#);
        assert_eq!(deserialized, original);
    }
}
