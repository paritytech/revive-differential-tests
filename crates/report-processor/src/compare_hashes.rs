use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
#[allow(unused_imports, reason = "only used in documentation")]
use revive_dt_common::types::Mode;

use crate::export_hashes::HashData;

/// A mismatch between the reference platform's hash and the other platform's hash.
#[derive(Clone, Debug)]
struct Mismatch {
    /// The normalized source path.
    path: String,
    /// The contract name.
    contract_name: String,
    /// The hash from the reference platform, or `None` if missing.
    reference_hash: Option<String>,
    /// The hash from the other platform, or `None` if missing.
    other_hash: Option<String>,
}

/// The result of the hash comparison.
#[derive(Clone, Debug)]
pub struct ComparisonResult {
    /// All platforms compared.
    pub platforms: Vec<String>,
    /// The reference platform used.
    pub reference_platform: String,
    /// The mismatches found in each compared platform, keyed by the
    /// [`Mode`] display string and the other/compared platform.
    pub mismatches: BTreeMap<String, BTreeMap<String, Vec<Mismatch>>>,
}

impl ComparisonResult {
    /// Counts the total number of mismatches across all modes.
    pub fn count_mismatches(&self) -> usize {
        self.mismatches
            .values()
            .flat_map(|mismatches_at_mode| mismatches_at_mode.values())
            .map(|mismatches_at_platform| mismatches_at_platform.len())
            .sum()
    }
}

/// Compares all platforms' hashes found in `all_hashes` across all `explicit_modes`
/// if provided, otherwise across all unique modes found.
pub fn compare_hashes(
    all_hashes: &[HashData],
    explicit_modes: Option<&[String]>,
) -> Result<ComparisonResult> {
    validate_hashes(all_hashes)?;

    let modes: Vec<String> = match explicit_modes {
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

    // Sort by platform to ensure deterministic reference selection.
    let mut all_hashes: Vec<&HashData> = all_hashes.iter().collect();
    all_hashes.sort_by_key(|hash_data| &hash_data.platform);

    let reference = all_hashes[0];
    let mut all_mismatches: BTreeMap<String, BTreeMap<String, Vec<Mismatch>>> = BTreeMap::new();

    for mode in &modes {
        let reference_hashes = reference.hashes.get(mode);

        for other in &all_hashes[1..] {
            let other_hashes = other.hashes.get(mode);
            let mismatches = compare(reference_hashes, other_hashes);

            all_mismatches
                .entry(mode.clone())
                .or_default()
                .insert(other.platform.clone(), mismatches);
        }
    }

    Ok(ComparisonResult {
        platforms: all_hashes
            .iter()
            .map(|hash_data| hash_data.platform.clone())
            .collect(),
        reference_platform: reference.platform.clone(),
        mismatches: all_mismatches,
    })
}

/// Compares the reference platform's hashes and the other platform's hashes.
fn compare(
    reference_hashes: Option<&BTreeMap<String, BTreeMap<String, String>>>,
    other_hashes: Option<&BTreeMap<String, BTreeMap<String, String>>>,
) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();

    let empty_map = BTreeMap::new();
    let reference_hashes = reference_hashes.unwrap_or(&empty_map);
    let other_hashes = other_hashes.unwrap_or(&empty_map);

    let source_paths: BTreeSet<&str> = reference_hashes
        .keys()
        .chain(other_hashes.keys())
        .map(|source_path| source_path.as_str())
        .collect();

    for source_path in source_paths {
        let reference_hashes = reference_hashes.get(source_path);
        let other_hashes = other_hashes.get(source_path);

        let contracts: BTreeSet<&str> = [reference_hashes, other_hashes]
            .into_iter()
            .flatten()
            .flat_map(|hashes| hashes.keys())
            .map(|contract_name| contract_name.as_str())
            .collect();

        for contract in contracts {
            let reference_hash = reference_hashes.and_then(|hashes| hashes.get(contract));
            let other_hash = other_hashes.and_then(|hashes| hashes.get(contract));

            if reference_hash != other_hash {
                mismatches.push(Mismatch {
                    path: source_path.to_string(),
                    contract_name: contract.to_string(),
                    reference_hash: reference_hash.cloned(),
                    other_hash: other_hash.cloned(),
                });
            }
        }
    }

    mismatches
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

/// Builds a human-readable comparison report from the [`ComparisonResult`].
pub fn build_comparison_report(result: &ComparisonResult) -> String {
    // TODO.
    format!(
        "Compared {} platforms ({}), {} total mismatches",
        result.platforms.len(),
        result.platforms.join(", "),
        result.count_mismatches()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates [`HashData`] with the given `entries` of `(mode, path, contract_name, hash)`.
    fn make_custom_hash_data(
        platform_label: &str,
        entries: &[(&str, &str, &str, &str)],
    ) -> HashData {
        let mut hashes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> =
            BTreeMap::new();
        for (mode, path, contract_name, hash) in entries {
            hashes
                .entry(mode.to_string())
                .or_default()
                .entry(path.to_string())
                .or_default()
                .insert(contract_name.to_string(), hash.to_string());
        }

        HashData {
            platform: platform_label.to_string(),
            hashes,
        }
    }

    /// Creates [`HashData`] where each provided mode has a single path and contract.
    fn make_simple_hash_data(platform_label: &str, modes: &[&str]) -> HashData {
        let mut hashes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> =
            BTreeMap::new();
        for mode in modes {
            hashes
                .entry(mode.to_string())
                .or_default()
                .entry("test.sol".to_string())
                .or_default()
                .insert("Test".to_string(), "0xabcd".to_string());
        }

        HashData {
            platform: platform_label.to_string(),
            hashes,
        }
    }

    /// Creates [`HashData`] with no hashes.
    fn make_empty_hash_data(platform_label: &str) -> HashData {
        HashData {
            platform: platform_label.to_string(),
            hashes: BTreeMap::new(),
        }
    }

    /// Asserts that `mismatches` contains an entry matching the given fields.
    fn assert_has_mismatch(
        mismatches: &[Mismatch],
        path: &str,
        contract_name: &str,
        reference_hash: Option<&str>,
        other_hash: Option<&str>,
    ) {
        assert!(
            mismatches.iter().any(|mismatch| {
                mismatch.path == path
                    && mismatch.contract_name == contract_name
                    && mismatch.reference_hash.as_deref() == reference_hash
                    && mismatch.other_hash.as_deref() == other_hash
            }),
            "Expected mismatch ({path}, {contract_name}, {reference_hash:?}, {other_hash:?})"
        );
    }

    #[test]
    fn compare_hashes_inferred_modes_ok() {
        let entries = &[
            ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
            ("Y M0 S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
            ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
            ("Y M3 S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ("Y Mz S+", "test1.sol", "Test1", "0x111111"),
            ("Y Mz S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y Mz S+", "test2.sol", "Test2", "0x222222"),
        ];
        let hashes_a = make_custom_hash_data("linux", entries);
        let hashes_b = make_custom_hash_data("macos", entries);
        let hashes_c = make_custom_hash_data("windows", entries);

        let result = compare_hashes(&[hashes_a, hashes_b, hashes_c], None).unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.count_mismatches(), 0);
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+", "Y M3 S+", "Y Mz S+"]
        );
    }

    #[test]
    fn compare_hashes_explicit_modes_ok() {
        let entries = &[
            ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
            ("Y M0 S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
            ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
            ("Y M3 S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ("Y Mz S+", "test1.sol", "Test1", "0x111111"),
            ("Y Mz S+", "test1.sol", "Test1B", "0x111bbb"),
            ("Y Mz S+", "test2.sol", "Test2", "0x222222"),
        ];
        let hashes_a = make_custom_hash_data("linux", entries);
        let hashes_b = make_custom_hash_data("macos", entries);
        let hashes_c = make_custom_hash_data("windows", entries);

        let modes = &[
            "Y M0 S+".to_string(),
            "Y M3 S+".to_string(),
            "Y Mz S+".to_string(),
        ];
        let result = compare_hashes(
            &[hashes_a.clone(), hashes_b.clone(), hashes_c.clone()],
            Some(modes),
        )
        .unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.count_mismatches(), 0);
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+", "Y M3 S+", "Y Mz S+"]
        );

        let modes = &["Y M0 S+".to_string()];
        let result = compare_hashes(&[hashes_a, hashes_b, hashes_c], Some(modes)).unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.count_mismatches(), 0);
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+"]
        );

        // Create a mismatch in `Y Mz S+` and request comparison of only `Y M0 S+`.
        let hashes_a = make_custom_hash_data(
            "linux",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y Mz S+", "test1.sol", "Test1", "0x111111"),
            ],
        );
        let hashes_b = make_custom_hash_data(
            "macos",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y Mz S+", "test1.sol", "Test1", "0xDIFFERENT"),
            ],
        );
        let modes = &["Y M0 S+".to_string()];
        let result = compare_hashes(&[hashes_a, hashes_b], Some(modes)).unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos"]);
        assert_eq!(result.count_mismatches(), 0);
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+"]
        );
    }

    #[test]
    fn compare_hashes_mismatch_different_hash() {
        let hashes_a = make_custom_hash_data(
            "linux",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M0 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
                ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M3 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ],
        );
        let hashes_b = make_custom_hash_data(
            "macos",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M0 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M0 S+", "test2.sol", "Test2", "0xDIFFERENT"),
                ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M3 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M3 S+", "test2.sol", "Test2", "0xDIFFERENT"),
            ],
        );
        let hashes_c = make_custom_hash_data(
            "windows",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0xDIFFERENT"),
                ("Y M0 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
                ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M3 S+", "test1.sol", "Test1B", "0x111bbb"),
                ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ],
        );

        let result = compare_hashes(&[hashes_a, hashes_b, hashes_c], None).unwrap();
        assert_eq!(result.count_mismatches(), 3);
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.reference_platform, "linux");
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+", "Y M3 S+"]
        );

        // Mode: Y M0 S+
        let macos_mismatches = &result.mismatches["Y M0 S+"]["macos"];
        assert_eq!(macos_mismatches.len(), 1);
        assert_has_mismatch(
            macos_mismatches,
            "test2.sol",
            "Test2",
            Some("0x222222"),
            Some("0xDIFFERENT"),
        );
        let windows_mismatches = &result.mismatches["Y M0 S+"]["windows"];
        assert_eq!(windows_mismatches.len(), 1);
        assert_has_mismatch(
            windows_mismatches,
            "test1.sol",
            "Test1",
            Some("0x111111"),
            Some("0xDIFFERENT"),
        );

        // Mode: Y M3 S+
        let macos_mismatches = &result.mismatches["Y M3 S+"]["macos"];
        assert_eq!(macos_mismatches.len(), 1);
        assert_has_mismatch(
            macos_mismatches,
            "test2.sol",
            "Test2",
            Some("0x222222"),
            Some("0xDIFFERENT"),
        );
        let windows_mismatches = &result.mismatches["Y M3 S+"]["windows"];
        assert!(windows_mismatches.is_empty());
    }

    #[test]
    fn compare_hashes_mismatch_missing_hash() {
        let hashes_a = make_custom_hash_data(
            "linux",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
                /* Y M3 S+ entirely missing on linux (the reference platform) */
            ],
        );
        let hashes_b = make_custom_hash_data(
            "macos",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
                ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ],
        );
        let hashes_c = make_custom_hash_data(
            "windows",
            &[
                ("Y M0 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M0 S+", "test2.sol", "Test2", "0x222222"),
                ("Y M3 S+", "test1.sol", "Test1", "0x111111"),
                ("Y M3 S+", "test2.sol", "Test2", "0x222222"),
            ],
        );

        let result = compare_hashes(&[hashes_a, hashes_b, hashes_c], None).unwrap();
        assert_eq!(result.count_mismatches(), 4);
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.reference_platform, "linux");
        assert_eq!(
            result.mismatches.keys().collect::<Vec<_>>(),
            vec!["Y M0 S+", "Y M3 S+"]
        );

        // Mode: Y M0 S+
        let macos_mismatches = &result.mismatches["Y M0 S+"]["macos"];
        assert!(macos_mismatches.is_empty());
        let windows_mismatches = &result.mismatches["Y M0 S+"]["windows"];
        assert!(windows_mismatches.is_empty());

        // Mode: Y M3 S+
        // Linux (the reference platform) has no data for this mode, so each compared
        // platform should report it as a mismatch.
        let macos_mismatches = &result.mismatches["Y M3 S+"]["macos"];
        assert_eq!(macos_mismatches.len(), 2);
        assert_has_mismatch(
            macos_mismatches,
            "test1.sol",
            "Test1",
            None,
            Some("0x111111"),
        );
        assert_has_mismatch(
            macos_mismatches,
            "test2.sol",
            "Test2",
            None,
            Some("0x222222"),
        );
        let windows_mismatches = &result.mismatches["Y M3 S+"]["windows"];
        assert_eq!(windows_mismatches.len(), 2);
        assert_has_mismatch(
            windows_mismatches,
            "test1.sol",
            "Test1",
            None,
            Some("0x111111"),
        );
        assert_has_mismatch(
            windows_mismatches,
            "test2.sol",
            "Test2",
            None,
            Some("0x222222"),
        );
    }

    #[test]
    fn compare_hashes_deterministic_reference() {
        // Do not order this list alphabetically, in order to correctly test reference selection.
        let hashes = vec![
            make_simple_hash_data("macos", &["Y M0 S+"]),
            make_simple_hash_data("windows", &["Y M0 S+"]),
            make_simple_hash_data("linux", &["Y M0 S+"]),
            make_simple_hash_data("wasm", &["Y M0 S+"]),
        ];

        let result = compare_hashes(&hashes, None).unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos", "wasm", "windows"]);
        assert_eq!(result.reference_platform, "linux");
    }

    #[test]
    fn validate_hashes_ok() {
        let hashes = vec![
            make_simple_hash_data("linux", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
            make_simple_hash_data("macos", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
        ];
        assert!(validate_hashes(&hashes).is_ok());

        let hashes = vec![
            make_simple_hash_data("linux", &["Y M0 S+", "Y M3 S+", "Y Mz S+"]),
            make_empty_hash_data("macos"),
        ];
        assert!(validate_hashes(&hashes).is_ok());
    }

    #[test]
    fn validate_hashes_too_few_sets() {
        let hashes = vec![make_simple_hash_data("linux", &["Y M3 S+"])];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("At least 2 sets of hashes are required for comparison, got 1"));
    }

    #[test]
    fn validate_hashes_empty_platform() {
        let hashes = vec![
            make_simple_hash_data("", &["Y M3 S+"]),
            make_simple_hash_data("macos", &["Y M3 S+"]),
        ];
        let error = validate_hashes(&hashes).unwrap_err().to_string();
        assert!(error.contains("Platform labels cannot be empty"));
    }

    #[test]
    fn validate_hashes_duplicate_platform() {
        let hashes = vec![
            make_simple_hash_data("linux", &["Y M3 S+"]),
            make_simple_hash_data("linux", &["Y M3 S+"]),
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
            make_simple_hash_data("linux", &["Y M0 S+", "Y M3 S+"]),
            make_simple_hash_data("macos", &["Y M0 S+"]),
        ];
        let modes = vec!["Y M3 S+".to_string()];
        assert!(validate_explicit_modes(&hashes, &modes).is_ok());
    }

    #[test]
    fn validate_explicit_modes_missing() {
        let hashes = vec![
            make_simple_hash_data("linux", &["Y M3 S+"]),
            make_simple_hash_data("macos", &["Y M3 S+"]),
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
            make_simple_hash_data("linux", &["Y M3 S+"]),
            make_simple_hash_data("macos", &["Y M3 S+"]),
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
            make_simple_hash_data("linux", &["Y M3 S+"]),
            make_simple_hash_data("macos", &["Y M3 S+"]),
        ];
        let modes = vec!["Y M0 S+".to_string()];
        let error = validate_explicit_modes(&hashes, &modes)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Mode 'Y M0 S+' not found in any of the provided sets of hashes"));
    }
}
