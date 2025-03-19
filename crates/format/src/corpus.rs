use std::{
    fs::File,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::metadata::Metadata;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Corpus {
    pub name: String,
    pub path: PathBuf,
}

impl Corpus {
    /// Try to read and parse the corpus definition file at given `path`.
    pub fn try_from_path(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)?;
        Ok(serde_json::from_reader(file)?)
    }

    /// Scan the corpus base directory and return all tests found.
    pub fn enumerate_tests(&self) -> Vec<Metadata> {
        let mut tests = Vec::new();
        collect_metadata(&self.path, &mut tests);
        tests
    }
}

/// Recursively walks `path` and parses any JSON or Solidity file into a test
/// definition [Metadata].
///
/// Found tests are inserted into `tests`.
///
/// `path` is expected to be a directory.
pub fn collect_metadata(path: &Path, tests: &mut Vec<Metadata>) {
    let dir_entry = match std::fs::read_dir(path) {
        Ok(dir_entry) => dir_entry,
        Err(error) => {
            log::error!("failed to read dir '{}': {error}", path.display());
            return;
        }
    };

    for entry in dir_entry {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::error!("error reading dir entry: {error}");
                continue;
            }
        };

        let path = entry.path();
        if path.is_dir() {
            collect_metadata(&path, tests);
            continue;
        }

        if path.is_file() {
            if let Some(metadata) = Metadata::try_from_file(&path) {
                tests.push(metadata)
            }
        }
    }
}
