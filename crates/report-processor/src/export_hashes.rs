use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
#[allow(unused_imports, reason = "only used in documentation")]
use revive_dt_common::types::Mode;
use revive_dt_config::Context;
use revive_dt_report::{CompilationStatus, CompiledContractInformation, Report};
use serde::{Deserialize, Serialize};

/// The bytecode hashes at a single mode, keyed by normalized source path and contract name.
pub type ModeHashData = BTreeMap<String, BTreeMap<String, String>>;

/// Hash data and the format used for export.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HashData {
    /// The platform to be associated with the hashes (e.g. "linux" or "macos").
    pub platform: String,
    /// The hash data keyed by the [`Mode`] display string.
    pub hashes: BTreeMap<String, ModeHashData>,
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

    /// Counts the number of hashes at a specific mode.
    pub fn count_hashes_at_mode(&self, mode: &str) -> usize {
        self.hashes
            .get(mode)
            .into_iter()
            .flat_map(|hashes_at_mode| hashes_at_mode.values())
            .map(|hashes_at_path| hashes_at_path.len())
            .sum()
    }
}

/// Extracts all bytecode hashes from pre-link compilations from a [`Report`].
pub fn extract_hashes(
    report: &Report,
    contracts_base_dir: &Path,
    platform_label: &str,
) -> Result<HashData> {
    let mut hashes: BTreeMap<String, ModeHashData> = BTreeMap::new();

    match &report.context {
        Context::Compile(_) => {
            for metadata_file_report in report.execution_information.values() {
                for (mode, compilation_report) in &metadata_file_report.compilation_reports {
                    if let Some(CompilationStatus::Success {
                        compiled_contracts_info,
                        ..
                    }) = &compilation_report.status
                    {
                        populate_hashes(
                            &mut hashes,
                            mode.to_string(),
                            compiled_contracts_info,
                            contracts_base_dir,
                        )?;
                    }
                }
            }
        }
        Context::Test(_) | Context::Benchmark(_) => {
            for metadata_file_report in report.execution_information.values() {
                for case_report in metadata_file_report.case_reports.values() {
                    for (mode, execution_report) in &case_report.mode_execution_reports {
                        for execution_info in execution_report.platform_execution.values().flatten()
                        {
                            if let Some(CompilationStatus::Success {
                                compiled_contracts_info,
                                ..
                            }) = &execution_info.post_link_compilation_status
                            {
                                populate_hashes(
                                    &mut hashes,
                                    mode.to_string(),
                                    compiled_contracts_info,
                                    contracts_base_dir,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        _ => bail!(
            "Extracting hashes is not supported for the context the report was generated with"
        ),
    }

    Ok(HashData {
        platform: platform_label.to_string(),
        hashes,
    })
}

/// Populates `hashes` with hashes found in `compiled_contracts_info`.
fn populate_hashes(
    hashes: &mut BTreeMap<String, ModeHashData>,
    mode_string: String,
    compiled_contracts_info: &HashMap<PathBuf, HashMap<String, CompiledContractInformation>>,
    contracts_base_dir: &Path,
) -> Result<()> {
    for (source_path, contracts_info_at_path) in compiled_contracts_info {
        let normalized_path = normalize_path(source_path, contracts_base_dir)?;

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

    Ok(())
}

/// Converts `path` to a relative path from the `base_path` and normalizes it with forward slashes.
///
/// Returns an error if `base_path` is not a base directory of `path`.
///
/// Purpose of normalization:
/// - **Consistent hash export keys**: During report processing, ensures the same contract maps
///   to the same key in exported hash files regardless of which platform produced the report,
///   allowing for hash comparison across platforms.
///
/// Example:
/// ```
/// # use std::path::Path;
/// # use revive_dt_common::fs::normalize_path;
/// let normalized_path = normalize_path(
///     Path::new("/home/runner/work/contracts/solidity/simple/loop.sol"),
///     Some(Path::new("/home/runner/work/contracts")),
/// ).unwrap();
/// assert_eq!(normalized_path, "solidity/simple/loop.sol");
///
/// let normalized_path = normalize_path(
///     Path::new("C:\\Users\\runner\\work\\contracts\\solidity\\simple\\loop.sol"),
///     Some(Path::new("C:\\Users\\runner\\work\\contracts")),
/// ).unwrap();
/// assert_eq!(normalized_path, "solidity/simple/loop.sol");
/// ```
fn normalize_path(path: &Path, base_path: &Path) -> Result<String> {
    // In order to be able to unit test Windows paths on non-Windows machines, string-based
    // prefix stripping is used here rather than `Path::strip_prefix()` because the
    // latter compares path components, and component parsing is platform-dependent
    // (on Unix, backslashes are not treated as separators).

    let path_string = path.to_string_lossy().replace('\\', "/");
    let base_string = format!(
        // Ensure base ends with `/` so only complete directory components are matched.
        // E.g. '/home/runner/fixtures' is not seen as a base dir of '/home/runner/fixturesABC/file.sol'.
        "{}/",
        base_path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
    );
    path_string
        .strip_prefix(&base_string)
        .map(|path_str| path_str.to_string())
        .with_context(|| {
            format!(
                "'{}' is not a base directory of path '{}'",
                base_path.display(),
                path.display(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        let normalized_unix_path = normalize_path(
            Path::new("/home/runner/fixtures/solidity/simple/default.sol"),
            Path::new("/home/runner/fixtures"),
        )
        .unwrap();
        let normalized_windows_path = normalize_path(
            Path::new("C:\\Users\\runner\\fixtures\\solidity\\simple\\default.sol"),
            Path::new("C:\\Users\\runner\\fixtures"),
        )
        .unwrap();

        assert_eq!(normalized_unix_path, normalized_windows_path);
        assert_eq!(normalized_unix_path, "solidity/simple/default.sol");
    }

    #[test]
    fn normalize_path_trailing_slash() {
        assert_eq!(
            normalize_path(
                Path::new("/home/runner/fixtures/solidity/simple/default.sol"),
                Path::new("/home/runner/fixtures/"),
            )
            .unwrap(),
            "solidity/simple/default.sol"
        );
    }

    #[test]
    fn normalize_path_invalid_base_path() {
        let error = normalize_path(
            Path::new("/other/path/file.sol"),
            Path::new("/home/runner/fixtures"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(
            "'/home/runner/fixtures' is not a base directory of path '/other/path/file.sol'"
        ));
    }

    #[test]
    fn normalize_path_invalid_partial_base_path() {
        let error = normalize_path(
            Path::new("/home/runner/fixturesABC/file.sol"),
            Path::new("/home/runner/fixtures"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(
            "'/home/runner/fixtures' is not a base directory of path '/home/runner/fixturesABC/file.sol'"
        ));
    }
}
