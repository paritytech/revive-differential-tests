use std::{
    fmt::Display,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParsedCompileSpecifier {
    /// All of the contracts in the file should be compiled.
    FileOrDirectory {
        /// The path of the metadata file containing the contracts or the references to the contracts.
        metadata_or_directory_file_path: PathBuf,
    },
}

impl ParsedCompileSpecifier {
    pub fn metadata_path(&self) -> &Path {
        match self {
            ParsedCompileSpecifier::FileOrDirectory {
                metadata_or_directory_file_path: metadata_file_path,
            } => metadata_file_path,
        }
    }
}

impl Display for ParsedCompileSpecifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedCompileSpecifier::FileOrDirectory {
                metadata_or_directory_file_path,
            } => {
                write!(f, "{}", metadata_or_directory_file_path.display())
            }
        }
    }
}

impl FromStr for ParsedCompileSpecifier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(s)
            .canonicalize()
            .context("Failed to canonicalize the path of the contracts")?;

        Ok(Self::FileOrDirectory {
            metadata_or_directory_file_path: path,
        })
    }
}

impl From<ParsedCompileSpecifier> for String {
    fn from(value: ParsedCompileSpecifier) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for ParsedCompileSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl TryFrom<&str> for ParsedCompileSpecifier {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Serialize for ParsedCompileSpecifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ParsedCompileSpecifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string = String::deserialize(deserializer)?;
        string.parse().map_err(serde::de::Error::custom)
    }
}
