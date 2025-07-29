//! This module downloads solc binaries.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use semver::{Version, VersionReq};
use sha2::{Digest, Sha256};

use crate::list::List;

pub static LIST_CACHE: LazyLock<Mutex<HashMap<&'static str, List>>> =
    LazyLock::new(Default::default);

impl List {
    pub const LINUX_URL: &str = "https://binaries.soliditylang.org/linux-amd64/list.json";
    pub const WINDOWS_URL: &str = "https://binaries.soliditylang.org/windows-amd64/list.json";
    pub const MACOSX_URL: &str = "https://binaries.soliditylang.org/macosx-amd64/list.json";
    pub const WASM_URL: &str = "https://binaries.soliditylang.org/wasm/list.json";

    /// Try to downloads the list from the given URL.
    ///
    /// Caches the list retrieved from the `url` into [LIST_CACHE],
    /// subsequent calls with the same `url` will return the cached list.
    pub fn download(url: &'static str) -> anyhow::Result<Self> {
        if let Some(list) = LIST_CACHE.lock().unwrap().get(url) {
            return Ok(list.clone());
        }

        let body: List = reqwest::blocking::get(url)?.json()?;

        LIST_CACHE.lock().unwrap().insert(url, body.clone());

        Ok(body)
    }
}

/// Download solc binaries from GitHub releases (IPFS links aren't reliable).
#[derive(Clone, Debug)]
pub struct GHDownloader {
    pub version: Version,
    pub target: &'static str,
    pub list: &'static str,
}

impl GHDownloader {
    pub const BASE_URL: &str = "https://github.com/ethereum/solidity/releases/download";

    pub const LINUX_NAME: &str = "solc-static-linux";
    pub const MACOSX_NAME: &str = "solc-macos";
    pub const WINDOWS_NAME: &str = "solc-windows.exe";
    pub const WASM_NAME: &str = "soljson.js";

    fn new(
        version: impl Into<VersionOrRequirement>,
        target: &'static str,
        list: &'static str,
    ) -> anyhow::Result<Self> {
        let version_or_requirement = version.into();
        match version_or_requirement {
            VersionOrRequirement::Version(version) => Ok(Self {
                version,
                target,
                list,
            }),
            VersionOrRequirement::Requirement(requirement) => {
                let Some(version) = List::download(list)?
                    .builds
                    .into_iter()
                    .map(|build| build.version)
                    .filter(|version| requirement.matches(version))
                    .max()
                else {
                    anyhow::bail!("Failed to find a version that satisfies {requirement:?}");
                };
                Ok(Self {
                    version,
                    target,
                    list,
                })
            }
        }
    }

    pub fn linux(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
        Self::new(version, Self::LINUX_NAME, List::LINUX_URL)
    }

    pub fn macosx(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
        Self::new(version, Self::MACOSX_NAME, List::MACOSX_URL)
    }

    pub fn windows(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
        Self::new(version, Self::WINDOWS_NAME, List::WINDOWS_URL)
    }

    pub fn wasm(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
        Self::new(version, Self::WASM_NAME, List::WASM_URL)
    }

    /// Returns the download link.
    pub fn url(&self) -> String {
        format!("{}/v{}/{}", Self::BASE_URL, &self.version, &self.target)
    }

    /// Download the solc binary.
    ///
    /// Errors out if the download fails or the digest of the downloaded file
    /// mismatches the expected digest from the release [List].
    pub fn download(&self) -> anyhow::Result<Vec<u8>> {
        tracing::info!("downloading solc: {self:?}");
        let expected_digest = List::download(self.list)?
            .builds
            .iter()
            .find(|build| build.version == self.version)
            .ok_or_else(|| anyhow::anyhow!("solc v{} not found builds", self.version))
            .map(|b| b.sha256.strip_prefix("0x").unwrap_or(&b.sha256).to_string())?;

        let file = reqwest::blocking::get(self.url())?.bytes()?.to_vec();

        if hex::encode(Sha256::digest(&file)) != expected_digest {
            anyhow::bail!("sha256 mismatch for solc version {}", self.version);
        }

        Ok(file)
    }
}

#[derive(Clone, Debug)]
pub enum VersionOrRequirement {
    Version(Version),
    Requirement(VersionReq),
}

impl From<Version> for VersionOrRequirement {
    fn from(value: Version) -> Self {
        Self::Version(value)
    }
}

impl From<VersionReq> for VersionOrRequirement {
    fn from(value: VersionReq) -> Self {
        Self::Requirement(value)
    }
}

#[cfg(test)]
mod tests {
    use crate::{download::GHDownloader, list::List};

    #[test]
    fn try_get_windows() {
        let version = List::download(List::WINDOWS_URL).unwrap().latest_release;
        GHDownloader::windows(version).unwrap().download().unwrap();
    }

    #[test]
    fn try_get_macosx() {
        let version = List::download(List::MACOSX_URL).unwrap().latest_release;
        GHDownloader::macosx(version).unwrap().download().unwrap();
    }

    #[test]
    fn try_get_linux() {
        let version = List::download(List::LINUX_URL).unwrap().latest_release;
        GHDownloader::linux(version).unwrap().download().unwrap();
    }

    #[test]
    fn try_get_wasm() {
        let version = List::download(List::WASM_URL).unwrap().latest_release;
        GHDownloader::wasm(version).unwrap().download().unwrap();
    }
}
