//! This module contains a parser for the Solidity semantic tests allowing them to be parsed into
//! regular [`Metadata`] objects that can be executed by the testing framework.
//!
//! [`Metadata`]: crate::metadata::Metadata

use std::{collections::VecDeque, path::PathBuf, str::FromStr};

use revive_common::EVMVersion;

use anyhow::{Context, Error, Result, anyhow, bail};

/// This enum describes the various sections that a semantic test can contain.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SemanticTestSection {
    /// A source code section that consists of Solidity code.
    ///
    /// Source code sections might have a file name and they might not. Take the following section
    /// as an example which doesn't contain a filename
    ///
    /// ```solidity
    /// contract C {
    ///     bytes data;
    ///     function () pure returns (bytes memory) f;
    ///     constructor() {
    ///         data = M.longdata();
    ///         f = M.longdata;
    ///     }
    ///     function test() public view returns (bool) {
    ///         return keccak256(data) == keccak256(f());
    ///     }
    /// }
    /// ```
    ///
    /// The above will translate into this enum variant and without a defined filename for the code.
    /// However, the following will translate into this variant of the enum with a defined file name
    ///
    /// ```solidity
    /// ==== Source: main.sol ====
    /// contract C {
    ///     bytes data;
    ///     function () pure returns (bytes memory) f;
    ///     constructor() {
    ///         data = M.longdata();
    ///         f = M.longdata;
    ///     }
    ///     function test() public view returns (bool) {
    ///         return keccak256(data) == keccak256(f());
    ///     }
    /// }
    /// ```
    ///
    /// This is because of the use of the `Source` directive at the start of the section.
    ///
    /// Note the following: All tests will be run on the last declared contract in the semantic test
    /// and therefore the order of the contracts matters.
    SourceCode {
        file_name: Option<PathBuf>,
        content: String,
    },

    /// An external source section from the solidity semantic tests.
    ///
    /// External source sections from the solidity semantic tests are the simplest sections out of
    /// them all. They look like the following:
    ///
    /// ```solidity
    /// ==== ExternalSource: _prbmath/PRBMathSD59x18.sol ====
    /// ```
    ///
    /// And they can be thought of as a directive to the compiler to include these contracts when
    /// compiling the test contract.
    ExternalSource { path: PathBuf },

    /// A test configuration section
    ///
    /// This section contains various configuration and filters that are used for the tests and its
    /// always the section that comes right before the actual tests. This section looks like the
    /// following:
    ///
    /// ```solidity
    /// // ====
    /// // ABIEncoderV1Only: true
    /// // compileViaYul: false
    /// // ----
    /// ```
    TestConfiguration { configuration: TestConfiguration },

    /// A test inputs section.
    ///
    /// This section consists of all of the lines that make up the test inputs or the test steps
    /// which is the final section found in the semantic test files. This section looks like the
    /// following:
    ///
    /// ```solidity
    /// // ----
    /// // f1() -> 0x20, 0x40, 0x20, 0
    /// // f2(string): 0x20, 0 -> 0x20, 0x40, 0x20, 0
    /// // f2(string): 0x20, 0, 0 -> 0x20, 0x40, 0x20, 0
    /// // g1() -> 32, 0
    /// // g2(string): 0x20, 0 -> 0x20, 0
    /// // g2(string): 0x20, 0, 0 -> 0x20, 0
    /// ```
    TestInputs { lines: Vec<String> },
}

impl SemanticTestSection {
    const SOURCE_SECTION_MARKER: &str = "==== Source:";
    const EXTERNAL_SOURCE_SECTION_MARKER: &str = "==== ExternalSource:";
    const TEST_CONFIGURATION_SECTION_MARKER: &str = "// ====";
    const TEST_INPUTS_SECTION_MARKER: &str = "// ----";

    pub fn parse_source_into_sections(source: impl AsRef<str>) -> Result<Vec<Self>> {
        let mut sections = VecDeque::<Self>::new();
        sections.push_back(Self::SourceCode {
            file_name: None,
            content: Default::default(),
        });

        for line in source.as_ref().split('\n') {
            if let Some(new_section) = sections
                .back_mut()
                .expect("Impossible case - we have at least one item in the sections")
                .append_line(line)?
            {
                sections.push_back(new_section);
            }
        }

        let first_section = sections
            .front()
            .expect("Impossible case - there's always at least one section");
        let remove_first_section = match first_section {
            SemanticTestSection::SourceCode { file_name, content } => {
                file_name.is_none() && content.is_empty()
            }
            SemanticTestSection::ExternalSource { .. }
            | SemanticTestSection::TestConfiguration { .. }
            | SemanticTestSection::TestInputs { .. } => false,
        };
        if remove_first_section {
            sections.pop_front();
        }

        Ok(sections.into_iter().collect())
    }

    /// Appends a line to a semantic test section.
    ///
    /// This method takes in the current section and a new line and attempts to append it to parse
    /// it and append it to the current section. If the line is found to be the start of a new
    /// section then no changes will be made to the current section and instead the line will be
    /// interpreted according to the rules of new sections.
    pub fn append_line(&mut self, line: impl AsRef<str>) -> Result<Option<Self>> {
        let line = line.as_ref();
        if line.is_empty() {
            Ok(None)
        } else if let Some(source_path) = line.strip_prefix(Self::SOURCE_SECTION_MARKER) {
            let source_code_file_path = source_path
                .trim()
                .split(' ')
                .next()
                .context("Failed to find the source code file path")?;
            Ok(Some(Self::SourceCode {
                file_name: Some(PathBuf::from(source_code_file_path)),
                content: Default::default(),
            }))
        } else if let Some(external_source_path) =
            line.strip_prefix(Self::EXTERNAL_SOURCE_SECTION_MARKER)
        {
            let source_code_file_path = external_source_path
                .trim()
                .split(' ')
                .next()
                .context("Failed to find the source code file path")?;
            Ok(Some(Self::ExternalSource {
                path: PathBuf::from(source_code_file_path),
            }))
        } else if line == Self::TEST_CONFIGURATION_SECTION_MARKER {
            Ok(Some(Self::TestConfiguration {
                configuration: Default::default(),
            }))
        } else if line == Self::TEST_INPUTS_SECTION_MARKER {
            Ok(Some(Self::TestInputs {
                lines: Default::default(),
            }))
        } else {
            match self {
                SemanticTestSection::SourceCode { content, .. } => {
                    content.push('\n');
                    content.push_str(line);
                    Ok(None)
                }
                SemanticTestSection::ExternalSource { .. } => Ok(Some(Self::SourceCode {
                    file_name: None,
                    content: line.to_owned(),
                })),
                SemanticTestSection::TestConfiguration { configuration } => {
                    let line = line
                        .strip_prefix("//")
                        .with_context(|| {
                            format!("Line doesn't contain test configuration prefix: {line}")
                        })?
                        .trim();
                    let mut splitted = line.split(':');
                    let key = splitted.next().context("Failed to find the key")?.trim();
                    let value = splitted.next().context("Failed to find the value")?.trim();
                    configuration.with_config(key, value)?;
                    Ok(None)
                }
                SemanticTestSection::TestInputs { lines } => {
                    let line = line
                        .strip_prefix("//")
                        .ok_or_else(|| anyhow!("Line doesn't contain test input prefix: {line}"))
                        .map(str::trim)?;
                    if !line.starts_with('#') {
                        lines.push(line.to_owned());
                    }
                    Ok(None)
                }
            }
        }
    }
}

/// The configuration parameters provided in the solidity semantic tests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TestConfiguration {
    /// Controls if the test case compiles through the Yul IR.
    compile_via_yul: Option<ItemConfig>,
    /// Controls if the compilation should be done to EWASM.
    compile_to_ewasm: Option<ItemConfig>,
    /// Controls if ABI encoding should be restricted to the V1 ABI encoder.
    abi_encoder_v1_only: Option<ItemConfig>,
    /// Controls the EVM Version that the test is compatible with.
    evm_version: Option<EvmVersionRequirement>,
    /// Controls how the revert strings should be handled.
    revert_strings: Option<RevertString>,
    /// Controls if non-existent functions should be permitted or not.
    allow_non_existing_functions: Option<bool>,
    /// The list of bytecode formats that this test should be run against.
    bytecode_format: Option<Vec<BytecodeFormat>>,
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_a_simple_file_correctly() {
        // Arrange
        const SIMPLE_FILE: &str = r#"
==== Source: main.sol ====
contract C {
    function f() public pure returns (uint) {
        return 1;
    }
}
// ====
// compileViaYul: true
// ----
// f() -> 1
"#;

        // Act
        let sections =
            SemanticTestSection::parse_source_into_sections(SIMPLE_FILE).expect("Failed to parse");

        // Assert
        assert_eq!(
            sections,
            vec![
                SemanticTestSection::SourceCode {
                    file_name: Some("main.sol".into()),
                    content: "\ncontract C {\n    function f() public pure returns (uint) {\n        return 1;\n    }\n}".to_string()
                },
                SemanticTestSection::TestConfiguration {
                    configuration: TestConfiguration { compile_via_yul: Some(true.into()), ..Default::default() },
                },
                SemanticTestSection::TestInputs {
                    lines: vec!["f() -> 1".to_string()]
                }
            ]
        )
    }

    #[test]
    fn parses_a_complex_file_correctly() {
        // Arrange
        const COMPLEX_FILE: &str = r#"
==== Source: main.sol ====
import "./lib.sol";
contract C {
    function f() public pure returns (uint) {
        return Lib.f();
    }
}
==== Source: lib.sol ====
library Lib {
    function f() internal pure returns (uint) {
        return 1;
    }
}
// ====
// compileViaYul: true
// ----
// # This is a comment
// f() -> 1
"#;

        // Act
        let sections =
            SemanticTestSection::parse_source_into_sections(COMPLEX_FILE).expect("Failed to parse");

        // Assert
        assert_eq!(
            sections,
            vec![
                SemanticTestSection::SourceCode {
                    file_name: Some("main.sol".into()),
                    content: "\nimport \"./lib.sol\";\ncontract C {\n    function f() public pure returns (uint) {\n        return Lib.f();\n    }\n}".to_string()
                },
                SemanticTestSection::SourceCode {
                    file_name: Some("lib.sol".into()),
                    content: "\nlibrary Lib {\n    function f() internal pure returns (uint) {\n        return 1;\n    }\n}".to_string()
                },
                SemanticTestSection::TestConfiguration {
                    configuration: TestConfiguration { compile_via_yul: Some(true.into()), ..Default::default() },
                },
                SemanticTestSection::TestInputs {
                    lines: vec!["f() -> 1".to_string()]
                }
            ]
        )
    }

    #[test]
    #[ignore = "Ignored and should be removed before making a PR"]
    fn test() {
        let files = revive_dt_common::iterators::FilesWithExtensionIterator::new(
            "/Users/omarabdulla/parity/resolc-compiler-tests/fixtures/solidity/ethereum",
        )
        .with_allowed_extension("sol");

        for file in files {
            let content = std::fs::read_to_string(file).unwrap();
            let sections = SemanticTestSection::parse_source_into_sections(content).unwrap();

            println!("{sections:#?}");
        }
    }
}
