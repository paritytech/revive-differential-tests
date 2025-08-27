use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use anyhow::Context;
use dashmap::DashMap;
use revive_dt_common::types::{ModePipeline, VersionOrRequirement};
use semver::{Version, VersionReq};

/// Return the path and version of a suitable `solc` compiler given the requirements provided.
///
/// This caches any compiler binaries/paths that are downloaded as a result of calling this.
pub async fn solc_compiler(
    cache_directory: &Path,
    fallback_version: &Version,
    required_version: Option<&VersionReq>,
    pipeline: ModePipeline,
) -> anyhow::Result<SolcCompiler> {
    // Require Yul compatible solc, or any if we don't care about compiling via Yul.
    let mut version_req = if pipeline == ModePipeline::ViaYulIR {
        solc_versions_supporting_yul_ir()
    } else {
        VersionReq::STAR
    };

    // Take into account the version requirements passed in, too.
    if let Some(other_version_req) = required_version {
        version_req
            .comparators
            .extend(other_version_req.comparators.iter().cloned());
    }

    // If no requirements yet then fall back to the fallback version.
    let version_req = if version_req == VersionReq::STAR {
        VersionOrRequirement::version_to_requirement(fallback_version)
    } else {
        version_req
    };

    // Download (or pull from cache) a suitable solc compiler given this.
    let solc_path =
        revive_dt_solc_binaries::download_solc(cache_directory, version_req, false).await?;
    let solc_version = solc_version(&solc_path).await?;

    Ok(SolcCompiler {
        version: solc_version,
        path: solc_path,
    })
}

/// A `solc` compiler, returned from [`solc_compiler`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolcCompiler {
    /// Version of the compiler.
    pub version: Version,
    /// Path to the compiler executable.
    pub path: PathBuf,
}

/// Fetch the solc version given a path to the executable
async fn solc_version(solc_path: &Path) -> anyhow::Result<semver::Version> {
    /// This is a cache of the path of the compiler to the version number of the compiler. We
    /// choose to cache the version in this way rather than through a field on the struct since
    /// compiler objects are being created all the time from the path and the compiler object is
    /// not reused over time.
    static VERSION_CACHE: LazyLock<DashMap<PathBuf, Version>> = LazyLock::new(Default::default);

    match VERSION_CACHE.entry(solc_path.to_path_buf()) {
        dashmap::Entry::Occupied(occupied_entry) => Ok(occupied_entry.get().clone()),
        dashmap::Entry::Vacant(vacant_entry) => {
            // The following is the parsing code for the version from the solc version strings
            // which look like the following:
            // ```
            // solc, the solidity compiler commandline interface
            // Version: 0.8.30+commit.73712a01.Darwin.appleclang
            // ```
            let child = Command::new(solc_path)
                .arg("--version")
                .stdout(Stdio::piped())
                .spawn()?;
            let output = child.wait_with_output()?;
            let output = String::from_utf8_lossy(&output.stdout);
            let version_line = output
                .split("Version: ")
                .nth(1)
                .context("Version parsing failed")?;
            let version_string = version_line
                .split("+")
                .next()
                .context("Version parsing failed")?;

            let version = Version::parse(version_string)?;

            vacant_entry.insert(version.clone());

            Ok(version)
        }
    }
}

/// This returns the solc versions which support Yul IR.
pub fn solc_versions_supporting_yul_ir() -> VersionReq {
    use semver::{Comparator, Op, Prerelease, VersionReq};
    VersionReq {
        comparators: vec![Comparator {
            op: Op::GreaterEq,
            major: 0,
            minor: Some(8),
            patch: Some(13),
            pre: Prerelease::EMPTY,
        }],
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn compiler_version_can_be_obtained() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("can create tempdir");
        let solc_path =
            revive_dt_solc_binaries::download_solc(temp_dir.path(), Version::new(0, 7, 6), false)
                .await
                .expect("can download solc");

        // Act
        let version = solc_version(&solc_path).await;

        // Assert
        assert_eq!(
            version.expect("Failed to get version"),
            Version::new(0, 7, 6)
        )
    }

    #[tokio::test]
    async fn compiler_version_can_be_obtained1() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("can create tempdir");
        let solc_path =
            revive_dt_solc_binaries::download_solc(temp_dir.path(), Version::new(0, 4, 21), false)
                .await
                .expect("can download solc");

        // Act
        let version = solc_version(&solc_path).await;

        // Assert
        assert_eq!(
            version.expect("Failed to get version"),
            Version::new(0, 4, 21)
        )
    }
}
