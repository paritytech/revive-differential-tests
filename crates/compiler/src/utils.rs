use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
};

use anyhow::Context;
use dashmap::DashMap;
use semver::Version;

/// Fetch the solc version given a path to the executable
pub async fn solc_version(solc_path: &Path) -> anyhow::Result<semver::Version> {
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
