use std::{
    fmt::Display,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};

use crate::types::Mode;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParsedTestSpecifier {
    /// All of the test cases in the file should be ran across all of the specified modes
    FileOrDirectory {
        /// The path of the metadata file containing the test cases.
        metadata_or_directory_file_path: PathBuf,
    },
    /// Only a specific case within the metadata file should be ran across all of the modes in the
    /// file.
    Case {
        /// The path of the metadata file containing the test cases.
        metadata_file_path: PathBuf,

        /// The index of the specific case to run.
        case_idx: usize,
    },
    /// A specific case and a specific mode should be ran. This is the most specific out of all of
    /// the specifier types.
    CaseWithMode {
        /// The path of the metadata file containing the test cases.
        metadata_file_path: PathBuf,

        /// The index of the specific case to run.
        case_idx: usize,

        /// The parsed mode that the test should be run in.
        mode: Mode,
    },
}

impl ParsedTestSpecifier {
    pub fn metadata_path(&self) -> &Path {
        match self {
            ParsedTestSpecifier::FileOrDirectory {
                metadata_or_directory_file_path: metadata_file_path,
            }
            | ParsedTestSpecifier::Case {
                metadata_file_path, ..
            }
            | ParsedTestSpecifier::CaseWithMode {
                metadata_file_path, ..
            } => metadata_file_path,
        }
    }
}

impl Display for ParsedTestSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedTestSpecifier::FileOrDirectory {
                metadata_or_directory_file_path,
            } => {
                write!(f, "{}", metadata_or_directory_file_path.display())
            }
            ParsedTestSpecifier::Case {
                metadata_file_path,
                case_idx,
            } => {
                write!(f, "{}::{}", metadata_file_path.display(), case_idx)
            }
            ParsedTestSpecifier::CaseWithMode {
                metadata_file_path,
                case_idx,
                mode,
            } => {
                write!(
                    f,
                    "{}::{}::{}",
                    metadata_file_path.display(),
                    case_idx,
                    mode
                )
            }
        }
    }
}

impl FromStr for ParsedTestSpecifier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut split_iter = s.split("::");

        let Some(path_string) = split_iter.next() else {
            bail!("Could not find the path in the test specifier")
        };
        let path = PathBuf::from(path_string)
            .canonicalize()
            .context("Failed to canonicalize the path of the test")?;

        let Some(case_idx_string) = split_iter.next() else {
            return Ok(Self::FileOrDirectory {
                metadata_or_directory_file_path: path,
            });
        };
        let case_idx = usize::from_str(case_idx_string)
            .context("Failed to parse the case idx of the test specifier from string")?;

        // At this point the provided path must be a file.
        if !path.is_file() {
            bail!(
                "Test specifier with a path and case idx must point to a file and not a directory"
            )
        }

        let Some(mode_string) = split_iter.next() else {
            return Ok(Self::Case {
                metadata_file_path: path,
                case_idx,
            });
        };
        let mode = Mode::from_str(mode_string)
            .context("Failed to parse the mode string in the parsed test specifier")?;

        Ok(Self::CaseWithMode {
            metadata_file_path: path,
            case_idx,
            mode,
        })
    }
}

impl From<ParsedTestSpecifier> for String {
    fn from(value: ParsedTestSpecifier) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for ParsedTestSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl TryFrom<&str> for ParsedTestSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Serialize for ParsedTestSpecifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ParsedTestSpecifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string = String::deserialize(deserializer)?;
        string.parse().map_err(serde::de::Error::custom)
    }
}
