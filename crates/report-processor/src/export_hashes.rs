use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use revive_dt_common::fs::normalize_path;
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

/// Extracts all bytecode hashes from post-link compilations from a [`Report`].
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
        let normalized_path = normalize_path(source_path, Some(contracts_base_dir))?;

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
