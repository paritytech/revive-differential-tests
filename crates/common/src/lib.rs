//! This crate provides common concepts, functionality, types, macros, and more that other crates in
//! the workspace can benefit from.

pub mod cached_fs;
pub mod fs;
pub mod futures;
pub mod iterators;
pub mod macros;
pub mod prelude {
    pub use crate::cached_fs::*;
    pub use crate::fs::*;
    pub use crate::futures::*;
    pub use crate::iterators::*;
    pub use crate::subscriptions::*;
    pub use crate::types::*;
}
pub mod subscriptions;
pub mod types;

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;

    pub use std::borrow::Cow;
    pub use std::collections::{BTreeMap, HashSet};
    pub use std::fmt::Display;
    pub use std::fs;
    pub use std::fs::{read_dir, remove_dir_all, remove_file};
    pub use std::io::Error as IoError;
    pub use std::io::Result as IoResult;
    pub use std::path::{Path, PathBuf};
    pub use std::str::FromStr;
    pub use std::sync::LazyLock;
    pub use std::sync::atomic::{AtomicUsize, Ordering};

    pub use alloy::primitives::{BlockNumber, BlockTimestamp, TxHash, U256};
    pub use alloy::signers::local::PrivateKeySigner;
    pub use anyhow::Context as _;
    pub use anyhow::{Result, bail};
    pub use clap::ValueEnum;
    pub use moka::sync::Cache;
    pub use once_cell::sync::Lazy;
    pub use regex::Regex;
    pub use schemars::JsonSchema;
    pub use semver::{Version, VersionReq};
    pub use serde::{Deserialize, Serialize};
    pub use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

    pub use crate::define_wrapper_type;
}
