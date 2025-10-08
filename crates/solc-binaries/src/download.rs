//! This module downloads solc binaries.

use std::{
	collections::HashMap,
	sync::{LazyLock, Mutex},
};

use revive_dt_common::types::VersionOrRequirement;

use semver::Version;
use sha2::{Digest, Sha256};

use crate::list::List;
use anyhow::Context as _;

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
	pub async fn download(url: &'static str) -> anyhow::Result<Self> {
		if let Some(list) = LIST_CACHE.lock().unwrap().get(url) {
			return Ok(list.clone());
		}

		let body: List = reqwest::get(url)
			.await
			.with_context(|| format!("Failed to GET solc list from {url}"))?
			.json()
			.await
			.with_context(|| format!("Failed to deserialize solc list JSON from {url}"))?;

		LIST_CACHE.lock().unwrap().insert(url, body.clone());

		Ok(body)
	}
}

/// Download solc binaries from the official SolidityLang site
#[derive(Clone, Debug)]
pub struct SolcDownloader {
	pub version: Version,
	pub target: &'static str,
	pub list: &'static str,
}

impl SolcDownloader {
	pub const BASE_URL: &str = "https://binaries.soliditylang.org";

	pub const LINUX_NAME: &str = "linux-amd64";
	pub const MACOSX_NAME: &str = "macosx-amd64";
	pub const WINDOWS_NAME: &str = "windows-amd64";
	pub const WASM_NAME: &str = "wasm";

	async fn new(
		version: impl Into<VersionOrRequirement>,
		target: &'static str,
		list: &'static str,
	) -> anyhow::Result<Self> {
		let version_or_requirement = version.into();
		match version_or_requirement {
			VersionOrRequirement::Version(version) => Ok(Self { version, target, list }),
			VersionOrRequirement::Requirement(requirement) => {
				let Some(version) = List::download(list)
					.await
					.with_context(|| format!("Failed to download solc builds list from {list}"))?
					.builds
					.into_iter()
					.map(|build| build.version)
					.filter(|version| requirement.matches(version))
					.max()
				else {
					anyhow::bail!("Failed to find a version that satisfies {requirement:?}");
				};
				Ok(Self { version, target, list })
			},
		}
	}

	pub async fn linux(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
		Self::new(version, Self::LINUX_NAME, List::LINUX_URL).await
	}

	pub async fn macosx(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
		Self::new(version, Self::MACOSX_NAME, List::MACOSX_URL).await
	}

	pub async fn windows(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
		Self::new(version, Self::WINDOWS_NAME, List::WINDOWS_URL).await
	}

	pub async fn wasm(version: impl Into<VersionOrRequirement>) -> anyhow::Result<Self> {
		Self::new(version, Self::WASM_NAME, List::WASM_URL).await
	}

	/// Download the solc binary.
	///
	/// Errors out if the download fails or the digest of the downloaded file
	/// mismatches the expected digest from the release [List].
	pub async fn download(&self) -> anyhow::Result<Vec<u8>> {
		let builds = List::download(self.list)
			.await
			.with_context(|| format!("Failed to download solc builds list from {}", self.list))?
			.builds;
		let build = builds
			.iter()
			.find(|build| build.version == self.version)
			.ok_or_else(|| anyhow::anyhow!("solc v{} not found builds", self.version))
			.with_context(|| {
				format!(
					"Requested solc version {} was not found in builds list fetched from {}",
					self.version, self.list
				)
			})?;

		let path = build.path.clone();
		let expected_digest = build.sha256.strip_prefix("0x").unwrap_or(&build.sha256).to_string();
		let url = format!("{}/{}/{}", Self::BASE_URL, self.target, path.display());

		let file = reqwest::get(&url)
			.await
			.with_context(|| format!("Failed to GET solc binary from {url}"))?
			.bytes()
			.await
			.with_context(|| format!("Failed to read solc binary bytes from {url}"))?
			.to_vec();

		if hex::encode(Sha256::digest(&file)) != expected_digest {
			anyhow::bail!("sha256 mismatch for solc version {}", self.version);
		}

		Ok(file)
	}
}

#[cfg(test)]
mod tests {
	use crate::{download::SolcDownloader, list::List};

	#[tokio::test]
	async fn try_get_windows() {
		let version = List::download(List::WINDOWS_URL).await.unwrap().latest_release;
		SolcDownloader::windows(version).await.unwrap().download().await.unwrap();
	}

	#[tokio::test]
	async fn try_get_macosx() {
		let version = List::download(List::MACOSX_URL).await.unwrap().latest_release;
		SolcDownloader::macosx(version).await.unwrap().download().await.unwrap();
	}

	#[tokio::test]
	async fn try_get_linux() {
		let version = List::download(List::LINUX_URL).await.unwrap().latest_release;
		SolcDownloader::linux(version).await.unwrap().download().await.unwrap();
	}

	#[tokio::test]
	async fn try_get_wasm() {
		let version = List::download(List::WASM_URL).await.unwrap().latest_release;
		SolcDownloader::wasm(version).await.unwrap().download().await.unwrap();
	}
}
