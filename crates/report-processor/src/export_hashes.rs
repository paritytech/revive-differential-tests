use std::collections::BTreeMap;
use std::path::Path;

#[allow(unused_imports, reason = "only used in documentation")]
use revive_dt_common::types::Mode;
use revive_dt_report::{CompilationStatus, Report};
use serde::{Deserialize, Serialize};

/// Hash data and the format used for export.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HashData {
    /// The platform to be associated with the hashes (e.g. "linux" or "macos").
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

/// Extracts all bytecode hashes from a report.
pub fn extract_hashes(
    report: &Report,
    contracts_base_dir: &Path,
    platform_label: &str,
) -> HashData {
    let mut hashes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> = BTreeMap::new();

    // TODO: Account for the report having been generated both via --compile and another command.
    // (Can determine from the "context" in the report.)

    for (_metadata_file_path, metadata_file_report) in &report.execution_information {
        for (mode, compilation_report) in &metadata_file_report.compilation_reports {
            if let Some(CompilationStatus::Success {
                compiled_contracts_info,
                ..
            }) = &compilation_report.status
            {
                let mode_string = mode.to_string();
                for (source_path, contracts_info_at_path) in compiled_contracts_info {
                    let normalized_path = normalize_path(source_path, contracts_base_dir);

                    for (contract_name, contract_info) in contracts_info_at_path {
                        hashes
                            .entry(mode_string.clone())
                            .or_default()
                            .entry(normalized_path.clone())
                            .or_default()
                            .insert(
                                contract_name.clone(),
                                format!("{}", contract_info.bytecode_hash),
                            );
                    }
                }
            }
        }
    }

    HashData {
        platform: platform_label.to_string(),
        hashes,
    }
}

/// Converts `path` to a relative path from the base directory and normalizes it with forward slashes.
/// This ensures that the normalized path added as part of the hash data is consistent, and thus
/// comparable, across platforms. It should thereby not be used to access the filesystem.
///
/// Example:
/// ```
/// path            = "/home/runner/work/contracts/solidity/simple/loop.sol"
/// base_dir        = "/home/runner/work/contracts"
/// normalized path = "solidity/simple/loop.sol"
/// ```
///
/// Example:
/// ```
/// path            = "C:\\Users\\runner\\work\\contracts\\solidity\\simple\\loop.sol"
/// base_dir        = "C:\\Users\\runner\\work\\contracts"
/// normalized path = "solidity/simple/loop.sol"
/// ```
fn normalize_path(path: &Path, base_dir: &Path) -> String {
    let remainder = path.strip_prefix(base_dir).unwrap_or_else(|_| {
        panic!(
            "The base directory '{}' is not a prefix of path '{}'",
            base_dir.display(),
            path.display(),
        )
    });

    remainder
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        let normalized_unix_path = normalize_path(
            Path::new("/home/runner/fixtures/solidity/simple/default.sol"),
            Path::new("/home/runner/fixtures"),
        );
        // TODO: Update normalize_path to allow testing Windows path strings on a linux.
        // let normalized_windows_path = normalize_path(
        //     Path::new("C:\\Users\\runner\\fixtures\\solidity\\simple\\default.sol"),
        //     Path::new("C:\\Users\\runner\\fixtures"),
        // );
        // assert_eq!(normalized_unix_path, normalized_windows_path,);
        assert_eq!(normalized_unix_path, "solidity/simple/default.sol");
    }

    #[test]
    fn normalize_path_trailing_slash() {
        assert_eq!(
            normalize_path(
                Path::new("/home/runner/fixtures/solidity/simple/default.sol"),
                Path::new("/home/runner/fixtures/")
            ),
            "solidity/simple/default.sol"
        );
    }

    #[test]
    #[should_panic(
        expected = "base directory '/home/runner/fixtures' is not a prefix of path '/other/path/file.sol'"
    )]
    fn normalize_path_invalid_prefix() {
        normalize_path(
            Path::new("/other/path/file.sol"),
            Path::new("/home/runner/fixtures"),
        );
    }
}
