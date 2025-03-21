//! This crates provides serializable Rust type definitions for the [solc binary lists][0].
//! The `download` feature enables helpers to download and cache solc binaries.
//!
//! [0]: https://binaries.soliditylang.org

#[cfg(feature = "download")]
pub mod download;
pub mod list;
