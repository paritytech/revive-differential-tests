use std::{
    fs::File,
    path::{Path, PathBuf},
};

use revive_dt_common::iterators::FilesWithExtensionIterator;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::metadata::{Metadata, MetadataFile};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Corpus {
    SinglePath { name: String, path: PathBuf },
    MultiplePaths { name: String, paths: Vec<PathBuf> },
}

impl Corpus {
    pub fn try_from_path(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut corpus = File::open(file_path.as_ref())
            .map_err(anyhow::Error::from)
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
        let mut tests = self
            .paths_iter()
            .flat_map(|root_path| {
                if !root_path.is_dir() {
                    Box::new(std::iter::once(root_path.to_path_buf()))
                        as Box<dyn Iterator<Item = _>>
                } else {
                    Box::new(
                        FilesWithExtensionIterator::new(root_path)
                            .with_use_cached_fs(true)
                            .with_allowed_extension("sol")
                            .with_allowed_extension("json"),
                    )
                }
                .map(move |metadata_file_path| (root_path, metadata_file_path))
            })
            .filter_map(|(root_path, metadata_file_path)| {
                Metadata::try_from_file(&metadata_file_path)
                    .or_else(|| {
                        debug!(
                            discovered_from = %root_path.display(),
                            metadata_file_path = %metadata_file_path.display(),
                            "Skipping file since it doesn't contain valid metadata"
                        );
                        None
                    })
                    .map(|metadata| MetadataFile {
                        metadata_file_path,
                        corpus_file_path: root_path.to_path_buf(),
                        content: metadata,
                    })
                    .inspect(|metadata_file| {
                        debug!(
                            metadata_file_path = %metadata_file.relative_path().display(),
                            "Loaded metadata file"
                        )
                    })
            })
            .collect::<Vec<_>>();
        tests.sort_by(|a, b| a.metadata_file_path.cmp(&b.metadata_file_path));
        tests.dedup_by(|a, b| a.metadata_file_path == b.metadata_file_path);
        info!(
            len = tests.len(),
            corpus_name = self.name(),
            "Found tests in Corpus"
        );
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

    pub fn path_count(&self) -> usize {
        match self {
            Corpus::SinglePath { .. } => 1,
            Corpus::MultiplePaths { paths, .. } => paths.len(),
        }
    }
}
