//! The test driver handles the compilation and execution of the test cases.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub mod compiler;
pub mod input;

pub(crate) fn contract_sources_from_metadata(
    directory: &Path,
    contracts: &BTreeMap<String, String>,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let mut sources = BTreeMap::new();
    for (name, contract) in contracts {
        // TODO: broken if a colon is in the dir name..
        let Some(solidity_file_name) = contract.split(':').next() else {
            anyhow::bail!("metadata contains invalid contract: {contract}");
        };

        let mut file = directory.to_path_buf();
        file.push(solidity_file_name);
        sources.insert(name.clone(), file);
    }

    Ok(sources)
}
