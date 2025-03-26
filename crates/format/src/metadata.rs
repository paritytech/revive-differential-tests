use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    case::Case,
    mode::{Mode, SolcMode},
};

pub const METADATA_FILE_EXTENSION: &str = "json";
pub const SOLIDITY_CASE_FILE_EXTENSION: &str = "sol";

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Metadata {
    pub cases: Vec<Case>,
    pub contracts: Option<BTreeMap<String, String>>,
    pub libraries: Option<BTreeMap<String, BTreeMap<String, String>>>,
    pub ignore: Option<bool>,
    pub modes: Option<Vec<Mode>>,
    pub file_path: Option<PathBuf>,
}

impl Metadata {
    /// Returns the solc modes of this metadata, inserting a default mode if not present.
    pub fn solc_modes(&self) -> Vec<SolcMode> {
        self.modes
            .to_owned()
            .unwrap_or_else(|| vec![Mode::Solidity(Default::default())])
            .iter()
            .filter_map(|mode| match mode {
                Mode::Solidity(solc_mode) => Some(solc_mode),
                Mode::Unknown(mode) => {
                    log::debug!("compiler: ignoring unknown mode '{mode}'");
                    None
                }
            })
            .cloned()
            .collect()
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

    /// Extract the contract sources.
    ///
    /// Returns a mapping of contract IDs to their source path and contract name.
    pub fn contract_sources(&self) -> anyhow::Result<BTreeMap<String, (PathBuf, String)>> {
        let directory = self.directory()?;
        let mut sources = BTreeMap::new();
        let Some(contracts) = &self.contracts else {
            return Ok(sources);
        };

        for (id, contract) in contracts {
            // TODO: broken if a colon is in the dir name..
            let mut parts = contract.split(':');
            let (Some(file_name), Some(contract_name)) = (parts.next(), parts.next()) else {
                anyhow::bail!("metadata contains invalid contract: {contract}");
            };
            let file = directory.to_path_buf().join(file_name);
            if !file.is_file() {
                anyhow::bail!("contract {id} is not a file: {}", file.display());
            }

            sources.insert(id.clone(), (file, contract_name.to_string()));
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

        let Some(file_extension) = path.extension() else {
            log::debug!("skipping corpus file: {}", path.display());
            return None;
        };

        if file_extension == METADATA_FILE_EXTENSION {
            return Self::try_from_json(path);
        }

        if file_extension == SOLIDITY_CASE_FILE_EXTENSION {
            return Self::try_from_solidity(path);
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
                metadata.file_path = Some(path.to_path_buf());
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

    fn try_from_solidity(_path: &Path) -> Option<Self> {
        None
    }
}
