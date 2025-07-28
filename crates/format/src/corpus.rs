use std::{
    fs::File,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::metadata::MetadataFile;

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq, Hash)]
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
    pub fn enumerate_tests(&self) -> Vec<MetadataFile> {
        let mut tests = Vec::new();
        collect_metadata(&self.path, &mut tests);
        tests
    }
}

/// If `path` is a file, try to parse it directly into a [MetadataFile].
/// Else, recursively walk through the `path` directory and parse any
/// JSON or Solidity file into a test definition [MetadataFile].
///
/// Found tests are inserted into `tests`.
pub fn collect_metadata(path: &Path, tests: &mut Vec<MetadataFile>) {
    if path.is_file()
        && let Some(metadata) = MetadataFile::try_from_file(&path)
    {
        tests.push(metadata);
        return;
    }

    let dir_entry = match std::fs::read_dir(path) {
        Ok(dir_entry) => dir_entry,
        Err(error) => {
            tracing::error!("failed to read dir '{}': {error}", path.display());
            return;
        }
    };

    for entry in dir_entry {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::error!("error reading dir entry: {error}");
                continue;
            }
        };

        let path = entry.path();
        collect_metadata(&path, tests);
    }
}
