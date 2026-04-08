use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
#[allow(unused_imports, reason = "only used in documentation")]
use revive_dt_common::types::Mode;
use serde::Serialize;

use crate::export_hashes::{HashData, ModeHashData};

/// A mismatch where not all platforms have the same bytecode hash for a specific contract.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Mismatch {
    /// The normalized source path.
    path: String,
    /// The contract name.
    contract_name: String,
    /// Each platform's hash for this contract, or `None` if missing.
    hashes: BTreeMap<String, Option<String>>,
}

impl Mismatch {
    /// Whether any platform is missing a hash for this contract.
    fn has_missing_hash(&self) -> bool {
        self.hashes.values().any(|hash| hash.is_none())
    }
}

/// The result of the hash comparison.
#[derive(Clone, Debug, Serialize)]
pub struct ComparisonResult {
    /// All platforms compared, listed in the deterministic order of comparison.
    pub platforms: Vec<String>,
    /// The number of hashes for each platform, keyed by the
    /// [`Mode`] display string and the platform.
    pub hash_counts: BTreeMap<String, BTreeMap<String, usize>>,
    /// The mismatches found, keyed by the [`Mode`] display string.
    pub mismatches: BTreeMap<String, Vec<Mismatch>>,
}

impl ComparisonResult {
    /// Counts the total number of mismatches across all modes.
    pub fn count_mismatches(&self) -> usize {
        self.mismatches
            .values()
            .map(|mismatches| mismatches.len())
            .sum()
    }

    /// Counts the total number of mismatches where any platform is missing a hash.
    pub fn count_mismatches_with_missing_hash(&self) -> usize {
        self.mismatches
            .values()
            .flatten()
            .filter(|mismatch| mismatch.has_missing_hash())
            .count()
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

    // Sort by platform to ensure deterministic ordering.
    let mut all_hashes: Vec<&HashData> = all_hashes.iter().collect();
    all_hashes.sort_by_key(|hash_data| &hash_data.platform);

    let mut all_hash_counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut all_mismatches: BTreeMap<String, Vec<Mismatch>> = BTreeMap::new();

    for mode in &modes {
        let hash_counts = all_hash_counts.entry(mode.clone()).or_default();
        for hash_data in &all_hashes {
            hash_counts.insert(
                hash_data.platform.clone(),
                hash_data.count_hashes_at_mode(mode),
            );
        }

        let mismatches = compare(&all_hashes, mode);
        all_mismatches.insert(mode.clone(), mismatches);
    }

    Ok(ComparisonResult {
        platforms: all_hashes
            .iter()
            .map(|hash_data| hash_data.platform.clone())
            .collect(),
        hash_counts: all_hash_counts,
        mismatches: all_mismatches,
    })
}

/// Compares all platforms' hashes at the given `mode`.
fn compare(all_hashes: &[&HashData], mode: &str) -> Vec<Mismatch> {
    let empty_map = BTreeMap::new();
    let mode_hash_data: Vec<(&str, &ModeHashData)> = all_hashes
        .iter()
        .map(|hash_data| {
            (
                hash_data.platform.as_str(),
                hash_data.hashes.get(mode).unwrap_or(&empty_map),
            )
        })
        .collect();

    let source_paths: BTreeSet<&str> = mode_hash_data
        .iter()
        .flat_map(|(_, hashes)| hashes.keys().map(|source_path| source_path.as_str()))
        .collect();

    let mut mismatches = Vec::new();
    for source_path in source_paths {
        let contracts: BTreeSet<&str> = mode_hash_data
            .iter()
            .filter_map(|(_, hashes)| hashes.get(source_path))
            .flat_map(|hashes| hashes.keys().map(|contract_name| contract_name.as_str()))
            .collect();

        for contract in contracts {
            let hashes_per_platform: BTreeMap<String, Option<String>> = mode_hash_data
                .iter()
                .map(|(platform, hashes)| {
                    let hash = hashes
                        .get(source_path)
                        .and_then(|hashes| hashes.get(contract))
                        .cloned();
                    (platform.to_string(), hash)
                })
                .collect();

            let mut hashes = hashes_per_platform.values();
            let first_hash = hashes.next().expect("platforms are validated to exist");
            let is_match = hashes.all(|other_hash| other_hash == first_hash);

            if !is_match {
                mismatches.push(Mismatch {
                    path: source_path.to_string(),
                    contract_name: contract.to_string(),
                    hashes: hashes_per_platform,
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

/// Builds a human-readable comparison summary from the [`ComparisonResult`].
pub fn build_comparison_summary(result: &ComparisonResult) -> String {
    let mut summaries_per_mode: Vec<String> = vec![];

    for (mode, mismatches) in &result.mismatches {
        let mut mode_summary = format!(
            "\n-------------------------------------------\
             \nMode: {mode}\
             \n-------------------------------------------"
        );

        let counts_info = result
            .platforms
            .iter()
            .map(|platform| {
                let count = result
                    .hash_counts
                    .get(mode)
                    .and_then(|counts| counts.get(platform))
                    .copied()
                    .unwrap_or(0);
                format!("{platform}: {count}")
            })
            .collect::<Vec<_>>()
            .join("\n    - ");
        let mismatch_count = mismatches.len();
        let missing_count = mismatches
            .iter()
            .filter(|mismatch| mismatch.has_missing_hash())
            .count();
        let missing_info = if missing_count > 0 {
            format!("(including {missing_count} missing hashes)")
        } else {
            String::new()
        };
        let status_symbol = if mismatch_count == 0 { "✅" } else { "❌" };

        mode_summary.push_str(&format!(
            "\n\
             \nHash counts:\
             \n    - {counts_info}\
             \n\
             \nMismatches: {status_symbol} {mismatch_count} {missing_info}"
        ));

        let max_displayed = 10;
        for mismatch in mismatches.iter().take(max_displayed) {
            mode_summary.push_str(&format!(
                "\n\
                 \n    - path: {}\
                 \n      contract: {}",
                mismatch.path, mismatch.contract_name,
            ));
            for (platform, hash) in &mismatch.hashes {
                mode_summary.push_str(&format!(
                    "\n      {platform}: {}",
                    hash.as_deref().unwrap_or("MISSING"),
                ));
            }
        }
        if mismatch_count > max_displayed {
            mode_summary.push_str(&format!(
                "\n\n    ... and {} more",
                mismatch_count - max_displayed
            ));
        }

        summaries_per_mode.push(mode_summary);
    }

    let platforms = format!("\n    - {}", result.platforms.join("\n    - "));
    let mode_reports = summaries_per_mode.join("\n");
    let modes_compared = format!(
        "\n    - {}",
        result
            .mismatches
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n    - ")
    );
    let total_mismatch_count = result.count_mismatches();
    let total_missing_count = result.count_mismatches_with_missing_hash();
    let missing_info = if total_missing_count > 0 {
        format!("(including {total_missing_count} missing hashes)")
    } else {
        String::new()
    };
    let status_message = if total_mismatch_count == 0 {
        "✅ SUCCESS: All hashes match across the compared platforms."
    } else {
        "❌ FAILURE: Mismatches found among the compared platforms!"
    };

    format!(
        "\n\
         ===========================================\n\
         COMPARISONS\n\
         ===========================================\n\
         {mode_reports}\n\
         \n\
         ===========================================\n\
         SUMMARY\n\
         ===========================================\n\
         \n\
         * Platforms compared: {platforms}\n\
         \n\
         * Modes compared: {modes_compared}\n\
         \n\
         Total mismatches for all modes: {total_mismatch_count} {missing_info}\n\
         \n\
         {status_message}\n\
         \n\
         ===========================================\n"
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
        expected_hashes: &[(&str, Option<&str>)],
    ) {
        let expected_hashes: BTreeMap<String, Option<String>> = expected_hashes
            .iter()
            .map(|(platform, hash)| (platform.to_string(), hash.map(String::from)))
            .collect();
        assert!(
            mismatches.iter().any(|mismatch| {
                mismatch.path == path
                    && mismatch.contract_name == contract_name
                    && mismatch.hashes == expected_hashes
            }),
            "Expected mismatch ({path}, {contract_name}, {expected_hashes:?})"
        );
    }

    /// Asserts the [`ComparisonResult`] against expected values, for the case where
    /// every platform is expected to have the same number of hashes at each mode.
    fn assert_expected_result(
        result: &ComparisonResult,
        expected_platforms: &[&str],
        expected_modes: &[&str],
        expected_hash_count_per_mode: usize,
        expected_mismatch_count: usize,
    ) {
        assert_eq!(result.platforms, expected_platforms);
        assert_eq!(result.count_mismatches(), expected_mismatch_count);
        assert_eq!(result.mismatches.keys().collect::<Vec<_>>(), expected_modes);
        assert_eq!(
            result.hash_counts.keys().collect::<Vec<_>>(),
            expected_modes
        );
        for mode in expected_modes {
            for platform in &result.platforms {
                assert_eq!(
                    result.hash_counts[*mode][platform],
                    expected_hash_count_per_mode
                );
            }
        }
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
        assert_expected_result(
            &result,
            &["linux", "macos", "windows"],
            &["Y M0 S+", "Y M3 S+", "Y Mz S+"],
            3,
            0,
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
        assert_expected_result(
            &result,
            &["linux", "macos", "windows"],
            &["Y M0 S+", "Y M3 S+", "Y Mz S+"],
            3,
            0,
        );

        let modes = &["Y M0 S+".to_string()];
        let result = compare_hashes(&[hashes_a, hashes_b, hashes_c], Some(modes)).unwrap();
        assert_expected_result(&result, &["linux", "macos", "windows"], &["Y M0 S+"], 3, 0);

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
        assert_expected_result(&result, &["linux", "macos"], &["Y M0 S+"], 1, 0);
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
        assert_expected_result(
            &result,
            &["linux", "macos", "windows"],
            &["Y M0 S+", "Y M3 S+"],
            3,
            3,
        );

        // Mode: Y M0 S+
        let mismatches = &result.mismatches["Y M0 S+"];
        assert_eq!(mismatches.len(), 2);
        assert_has_mismatch(
            mismatches,
            "test1.sol",
            "Test1",
            &[
                ("linux", Some("0x111111")),
                ("macos", Some("0x111111")),
                ("windows", Some("0xDIFFERENT")),
            ],
        );
        assert_has_mismatch(
            mismatches,
            "test2.sol",
            "Test2",
            &[
                ("linux", Some("0x222222")),
                ("macos", Some("0xDIFFERENT")),
                ("windows", Some("0x222222")),
            ],
        );

        // Mode: Y M3 S+
        let mismatches = &result.mismatches["Y M3 S+"];
        assert_eq!(mismatches.len(), 1);
        assert_has_mismatch(
            mismatches,
            "test2.sol",
            "Test2",
            &[
                ("linux", Some("0x222222")),
                ("macos", Some("0xDIFFERENT")),
                ("windows", Some("0x222222")),
            ],
        );
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
        assert_eq!(result.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(result.count_mismatches(), 2);
        let expected_modes = vec!["Y M0 S+", "Y M3 S+"];
        assert_eq!(result.mismatches.keys().collect::<Vec<_>>(), expected_modes);
        assert_eq!(
            result.hash_counts.keys().collect::<Vec<_>>(),
            expected_modes
        );
        assert_eq!(result.hash_counts["Y M0 S+"]["linux"], 2);
        assert_eq!(result.hash_counts["Y M0 S+"]["macos"], 2);
        assert_eq!(result.hash_counts["Y M0 S+"]["windows"], 2);
        assert_eq!(result.hash_counts["Y M3 S+"]["linux"], 0);
        assert_eq!(result.hash_counts["Y M3 S+"]["macos"], 2);
        assert_eq!(result.hash_counts["Y M3 S+"]["windows"], 2);

        // Mode: Y M0 S+
        let mismatches = &result.mismatches["Y M0 S+"];
        assert!(mismatches.is_empty());

        // Mode: Y M3 S+
        // Linux has no data for this mode, so each contract is a mismatch.
        let mismatches = &result.mismatches["Y M3 S+"];
        assert_eq!(mismatches.len(), 2);
        assert_has_mismatch(
            mismatches,
            "test1.sol",
            "Test1",
            &[
                ("linux", None),
                ("macos", Some("0x111111")),
                ("windows", Some("0x111111")),
            ],
        );
        assert_has_mismatch(
            mismatches,
            "test2.sol",
            "Test2",
            &[
                ("linux", None),
                ("macos", Some("0x222222")),
                ("windows", Some("0x222222")),
            ],
        );
    }

    #[test]
    fn compare_hashes_deterministic_platform_order() {
        // Do not order this list alphabetically, in order to correctly test platform ordering.
        let hashes = vec![
            make_simple_hash_data("macos", &["Y M0 S+"]),
            make_simple_hash_data("windows", &["Y M0 S+"]),
            make_simple_hash_data("linux", &["Y M0 S+"]),
            make_simple_hash_data("wasm", &["Y M0 S+"]),
        ];

        let result = compare_hashes(&hashes, None).unwrap();
        assert_eq!(result.platforms, vec!["linux", "macos", "wasm", "windows"]);
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
