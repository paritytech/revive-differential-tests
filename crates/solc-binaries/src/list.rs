//! Rust type definitions for the solc binary lists.

use std::{collections::HashMap, path::PathBuf};

use semver::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, Eq, PartialEq)]
pub struct List {
    pub builds: Vec<Build>,
    pub releases: HashMap<Version, String>,
    #[serde(rename = "latestRelease")]
    pub latest_release: Version,
}

#[derive(Debug, Deserialize, Clone, Eq, PartialEq)]
pub struct Build {
    pub path: PathBuf,
    pub version: Version,
    pub build: String,
    #[serde(rename = "longVersion")]
    pub long_version: String,
    pub keccak256: String,
    pub sha256: String,
    pub urls: Vec<String>,
}
