use std::collections::BTreeSet;

use anyhow::{Result, bail};

use crate::export_hashes::HashData;

/// Compares all platforms' hashes found in `all_hashes` across all `explicit_modes`
/// if provided, otherwise across all unique modes found.
pub fn compare_hashes(all_hashes: &[HashData], explicit_modes: Option<&[String]>) -> Result<()> {
    validate_hashes(all_hashes)?;

    let _modes: Vec<String> = match explicit_modes {
        Some(modes) => {
            validate_explicit_modes(all_hashes, modes)?;
            modes.to_vec()
        }
        None => all_hashes
            .iter()
            .flat_map(|platform_hash_data| platform_hash_data.hashes.keys().cloned())
            // Deduplicate mode keys.
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    };

    // TODO.

    Ok(())
}

/// Validates that all hash data meet the requirements needed for comparison.
fn validate_hashes(hashes: &[HashData]) -> Result<()> {
    if hashes.len() < 2 {
        bail!(
            "At least 2 sets of hashes are required for comparison, got {}",
            hashes.len()
        );
    }

    let mut seen_platforms: BTreeSet<&str> = BTreeSet::new();
    let mut total_hashes: usize = 0;

    for platform_hash_data in hashes {
        if platform_hash_data.platform.is_empty() {
            bail!("Platform labels cannot be empty");
        }

        if !seen_platforms.insert(&platform_hash_data.platform) {
            bail!(
                "Duplicate platform label found: '{}'",
                platform_hash_data.platform,
            );
        }

        total_hashes += platform_hash_data.count_hashes();
    }

    if total_hashes == 0 {
        bail!("No hashes found");
    }

    Ok(())
}

/// Validates that the explicitly provided modes meet the requirements needed for comparison.
fn validate_explicit_modes(hashes: &[HashData], modes: &[String]) -> Result<()> {
    if modes.is_empty() {
        bail!("If requesting explicit modes to compare, at least one mode must be provided");
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for mode in modes {
        if !seen.insert(mode.as_str()) {
            bail!("The modes requested to compare has a duplicate: '{mode}'");
        }

        // For running the comparisons, it is okay if a mode only exists for
        // one of the platforms. If it is missing for other platforms, it will
        // instead be reported in the comparison result as a mismatch.
        let exists_for_any_platform = hashes
            .iter()
            .any(|platform_hash_data| platform_hash_data.hashes.contains_key(mode));
        if !exists_for_any_platform {
            bail!("Mode '{mode}' not found in any of the provided sets of hashes");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_hash_data(platform_label: &str, modes: &[&str]) -> HashData {
        let mut hashes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> =
            BTreeMap::new();
        for mode in modes {
            hashes
                .entry(mode.to_string())
                .or_default()
                .entry("file.sol".to_string())
                .or_default()
                .insert("Contract".to_string(), "0xabcd".to_string());
        }

        HashData {
            platform: platform_label.to_string(),
            hashes,
        }
    }

    fn make_empty_hash_data(platform_label: &str) -> HashData {
        HashData {
            platform: platform_label.to_string(),
            hashes: BTreeMap::new(),
        }
    }

    #[test]
    fn validate_hashes_ok() {
        let hashes = vec![
            make_hash_data("linux", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
            make_hash_data("macos", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
        ];
        assert!(validate_hashes(&hashes).is_ok());

        let hashes = vec![
            make_hash_data("linux", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
            make_empty_hash_data("macos"),
        ];
        assert!(validate_hashes(&hashes).is_ok());
    }

    #[test]
    fn validate_hashes_too_few_sets() {
        let hashes = vec![make_hash_data("linux", &["Y M3 S+"])];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("At least 2 sets of hashes are required for comparison, got 1"));
    }

    #[test]
    fn validate_hashes_empty_platform() {
        let hashes = vec![
            make_hash_data("", &["Y M3 S+"]),
            make_hash_data("macos", &["Y M3 S+"]),
        ];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("Platform labels cannot be empty"));
    }

    #[test]
    fn validate_hashes_duplicate_platform() {
        let hashes = vec![
            make_hash_data("linux", &["Y M3 S+"]),
            make_hash_data("linux", &["Y M3 S+"]),
        ];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("Duplicate platform label found: 'linux'"));
    }

    #[test]
    fn validate_hashes_no_hashes() {
        let hashes = vec![make_empty_hash_data("linux"), make_empty_hash_data("macos")];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("No hashes found"));
    }

    #[test]
    fn validate_explicit_modes_ok() {
        let hashes = vec![
            make_hash_data("linux", &["Y M0 S+", "Y M3 S+"]),
            make_hash_data("macos", &["Y M0 S+"]),
        ];
        let modes = vec!["Y M3 S+".to_string()];
        assert!(validate_explicit_modes(&hashes, &modes).is_ok());
    }

    #[test]
    fn validate_explicit_modes_missing() {
        let hashes = vec![
            make_hash_data("linux", &["Y M3 S+"]),
            make_hash_data("macos", &["Y M3 S+"]),
        ];
        let modes: Vec<String> = vec![];
        let error = validate_explicit_modes(&hashes, &modes)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "If requesting explicit modes to compare, at least one mode must be provided"
        ));
    }

    #[test]
    fn validate_explicit_modes_duplicate() {
        let hashes = vec![
            make_hash_data("linux", &["Y M3 S+"]),
            make_hash_data("macos", &["Y M3 S+"]),
        ];
        let modes = vec!["Y M3 S+".to_string(), "Y M3 S+".to_string()];
        let error = validate_explicit_modes(&hashes, &modes)
            .unwrap_err()
            .to_string();
        assert!(error.contains("The modes requested to compare has a duplicate: 'Y M3 S+'"));
    }

    #[test]
    fn validate_explicit_modes_not_found() {
        let hashes = vec![
            make_hash_data("linux", &["Y M3 S+"]),
            make_hash_data("macos", &["Y M3 S+"]),
        ];
        let modes = vec!["Y M0 S+".to_string()];
        let error = validate_explicit_modes(&hashes, &modes)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Mode 'Y M0 S+' not found in any of the provided sets of hashes"));
    }
}
