use std::{
    fs::File,
    path::{Path, PathBuf},
};

use revive_dt_common::cached_fs::read_dir;
use serde::{Deserialize, Serialize};

use crate::metadata::MetadataFile;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Corpus {
    SinglePath { name: String, path: PathBuf },
    MultiplePaths { name: String, paths: Vec<PathBuf> },
}

impl Corpus {
    pub fn try_from_path(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut corpus = File::open(file_path.as_ref())
            .map_err(Into::<anyhow::Error>::into)
            .and_then(|file| serde_json::from_reader::<_, Corpus>(file).map_err(Into::into))?;

        for path in corpus.paths_iter_mut() {
            *path = file_path
                .as_ref()
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
                .join(path.as_path())
        }

        Ok(corpus)
    }

    pub fn enumerate_tests(&self) -> Vec<MetadataFile> {
        let mut tests = Vec::new();
        for path in self.paths_iter() {
            collect_metadata(path, &mut tests);
        }
        tests
    }

    pub fn name(&self) -> &str {
        match self {
            Corpus::SinglePath { name, .. } | Corpus::MultiplePaths { name, .. } => name.as_str(),
        }
    }

    pub fn paths_iter(&self) -> impl Iterator<Item = &Path> {
        match self {
            Corpus::SinglePath { path, .. } => {
                Box::new(std::iter::once(path.as_path())) as Box<dyn Iterator<Item = _>>
            }
            Corpus::MultiplePaths { paths, .. } => {
                Box::new(paths.iter().map(|path| path.as_path())) as Box<dyn Iterator<Item = _>>
            }
        }
    }

    pub fn paths_iter_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        match self {
            Corpus::SinglePath { path, .. } => {
                Box::new(std::iter::once(path)) as Box<dyn Iterator<Item = _>>
            }
            Corpus::MultiplePaths { paths, .. } => {
                Box::new(paths.iter_mut()) as Box<dyn Iterator<Item = _>>
            }
        }
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
        let dir_entry = match read_dir(path) {
            Ok(dir_entry) => dir_entry,
            Err(error) => {
                tracing::error!("failed to read dir '{}': {error}", path.display());
                return;
            }
        };

        for path in dir_entry {
            let path = match path {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::error!("error reading dir entry: {error}");
                    continue;
                }
            };

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
