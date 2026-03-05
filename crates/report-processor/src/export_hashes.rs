use std::collections::BTreeMap;

#[allow(unused_imports, reason = "only used in documentation")]
use revive_dt_common::types::Mode;
use serde::{Deserialize, Serialize};

/// Hash data and the format used for export.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HashData {
    /// The platform to be associated with the hashes (e.g. "linux", "macos").
    pub platform: String,
    /// The bytecode hashes keyed by the [`Mode`] display string, normalized source path, and contract name.
    pub hashes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
}

impl HashData {
    /// Counts the total number of hashes across all modes.
    pub fn count_hashes(&self) -> usize {
        self.hashes
            .values()
            .flat_map(|hashes_at_mode| hashes_at_mode.values())
            .map(|hashes_at_path| hashes_at_path.len())
            .sum()
    }
}
