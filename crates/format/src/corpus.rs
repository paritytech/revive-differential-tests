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
        let mut corpus: Corpus = serde_json::from_reader(file)?;

        // Ensure that the path mentioned in the corpus is relative to the corpus file.
        // Canonicalizing also helps make the path in any errors unambiguous.
        corpus.path = path
            .parent()
            .ok_or_else(|| {
                anyhow::anyhow!("Corpus path '{}' does not point to a file", path.display())
            })?
            .canonicalize()
            .map_err(|error| {
                anyhow::anyhow!(
                    "Failed to canonicalize path to corpus '{}': {error}",
                    path.display()
                )
            })?
            .join(corpus.path);

        Ok(corpus)
    }

    /// Scan the corpus base directory and return all tests found.
    pub fn enumerate_tests(&self) -> Vec<MetadataFile> {
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
pub fn collect_metadata(path: &Path, tests: &mut Vec<MetadataFile>) {
    if path.is_dir() {
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
            if path.is_dir() {
                collect_metadata(&path, tests);
                continue;
            }

            if path.is_file() {
                if let Some(metadata) = MetadataFile::try_from_file(&path) {
                    tests.push(metadata)
                }
            }
        }
    } else {
        let Some(extension) = path.extension() else {
            tracing::error!("Failed to get file extension");
            return;
        };
        if extension.eq_ignore_ascii_case("sol") || extension.eq_ignore_ascii_case("json") {
            if let Some(metadata) = MetadataFile::try_from_file(path) {
                tests.push(metadata)
            }
        } else {
            tracing::error!(?extension, "Unsupported file extension");
        }
    }
}
