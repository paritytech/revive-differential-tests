use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{case::Case, mode::Mode};

pub const METADATA_FILE_EXTENSION: &str = "json";
pub const SOLIDITY_CASE_FILE_EXTENSION: &str = "sol";

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Metadata {
    pub cases: Vec<Case>,
    pub contracts: Option<BTreeMap<String, String>>,
    pub libraries: Option<BTreeMap<String, BTreeMap<String, String>>>,
    pub ignore: Option<bool>,
    pub modes: Option<Vec<Mode>>,
    pub path: Option<PathBuf>,
    pub directory: Option<PathBuf>,
}

impl Metadata {
    /// Try to parse the test metadata struct from the given file at `path`.
    ///
    /// Returns `None` if `path` didn't contain a test metadata or case definition.
    ///
    /// # Panics
    /// Expects the supplied `path` to be a file.
    pub fn try_from_file(path: &Path) -> Option<Self> {
        assert!(path.is_file(), "not a file: {}", path.display());

        let Some(file_extension) = path.extension() else {
            log::debug!("skipping corpus file: {}", path.display());
            return None;
        };

        if file_extension == METADATA_FILE_EXTENSION {
            return Self::try_from_json(path);
        }

        if file_extension == SOLIDITY_CASE_FILE_EXTENSION {
            return None;
            //return Self::try_from_solidity(path);
        }

        log::debug!("ignoring invalid corpus file: {}", path.display());
        None
    }

    fn try_from_json(path: &Path) -> Option<Self> {
        let file = File::open(path)
            .inspect_err(|error| {
                log::error!(
                    "opening JSON test metadata file '{}' error: {error}",
                    path.display()
                );
            })
            .ok()?;

        match serde_json::from_reader::<_, Metadata>(file) {
            Ok(mut metadata) => {
                metadata.directory = Some(path.to_path_buf());
                Some(metadata)
            }
            Err(error) => {
                log::error!(
                    "parsing JSON test metadata file '{}' error: {error}",
                    path.display()
                );
                None
            }
        }
    }

    fn try_from_solidity(path: &Path) -> Option<Self> {
        todo!()
    }
}
