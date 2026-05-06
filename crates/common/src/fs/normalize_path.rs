use crate::internal_prelude::*;

/// Converts `path` to a relative path from the `base_path` and normalizes it with forward
/// slashes. If `base_path` is `None`, the original path is returned with forward slashes.
///
/// Returns an error if `base_path` is `Some` but is not a base directory of `path`.
///
/// Purpose of normalization:
/// 1. **Correct import resolution on Windows**: Solc's standard JSON mode stores source keys verbatim
///    but resolves relative imports after normalizing to forward slashes, causing source lookup
///    misses (e.g. trying to look up `D:/a/fixtures/lib.sol` instead of `D:\a\fixtures\lib.sol`).
///    This in turn causes duplicate declaration errors and library linker symbol mismatches
///    (see [Solidity Issue #16579](https://github.com/argotorg/solidity/issues/16579)).
///    We therefore normalize the standard JSON input source paths in `sources` and `libraries`,
///    allowing more contracts to be compiled.
/// 2. **Consistent hash export keys**: During report processing, ensures the same contract maps
///    to the same key in exported hash files regardless of which platform produced the report,
///    allowing for hash comparison across platforms.
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
pub fn normalize_path(path: &Path, base_path: Option<&Path>) -> Result<String> {
    // In order to be able to unit test Windows paths on non-Windows machines, string-based
    // prefix stripping is used here rather than `Path::strip_prefix()` because the
    // latter compares path components, and component parsing is platform-dependent
    // (on Unix, backslashes are not treated as separators).

    let path_string = path.to_string_lossy().replace('\\', "/");
    let Some(base) = base_path else {
        return Ok(path_string);
    };
    let base_string = format!(
        // Ensure base ends with `/` so only complete directory components are matched.
        // E.g. '/home/runner/fixtures' is not seen as a base dir of '/home/runner/fixturesABC/file.sol'.
        "{}/",
        base.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
    );
    path_string
        .strip_prefix(&base_string)
        .map(|path_str| path_str.to_string())
        .with_context(|| {
            format!(
                "'{}' is not a base directory of path '{}'",
                base.display(),
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
            Some(Path::new("/home/runner/fixtures")),
        )
        .unwrap();
        let normalized_windows_path = normalize_path(
            Path::new("C:\\Users\\runner\\fixtures\\solidity\\simple\\default.sol"),
            Some(Path::new("C:\\Users\\runner\\fixtures")),
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
                Some(Path::new("/home/runner/fixtures/")),
            )
            .unwrap(),
            "solidity/simple/default.sol"
        );
    }

    #[test]
    fn normalize_path_extended_length() {
        assert_eq!(
            normalize_path(
                Path::new("\\\\?\\C:\\Users\\runner\\fixtures\\solidity\\simple\\default.sol"),
                Some(Path::new("\\\\?\\C:\\Users\\runner\\fixtures")),
            )
            .unwrap(),
            "solidity/simple/default.sol"
        );
    }

    #[test]
    fn normalize_path_no_base_path() {
        assert_eq!(
            normalize_path(
                Path::new("/home/runner/fixtures/solidity/simple/default.sol"),
                None,
            )
            .unwrap(),
            "/home/runner/fixtures/solidity/simple/default.sol"
        );
    }

    #[test]
    fn normalize_path_invalid_base_path() {
        let error = normalize_path(
            Path::new("/other/path/file.sol"),
            Some(Path::new("/home/runner/fixtures")),
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
            Some(Path::new("/home/runner/fixtures")),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(
            "'/home/runner/fixtures' is not a base directory of path '/home/runner/fixturesABC/file.sol'"
        ));
    }
}
