//! This crate provides common concepts, functionality, types, macros, and more that other crates in
//! the workspace can benefit from.

pub mod cached_fs;
pub mod fs;
pub mod futures;
pub mod iterators;
pub mod macros;
pub mod subscriptions;
pub mod types;

pub mod prelude {
    pub use crate::{cached_fs::*, fs::*, futures::*, iterators::*, subscriptions::*, types::*};
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;

    pub use std::{
        borrow::Cow,
        collections::{BTreeMap, HashMap, HashSet},
        fmt::Display,
        fs,
        hash::Hash,
        io::{Error as IoError, Result as IoResult},
        path::{Path, PathBuf},
        str::FromStr,
        sync::{Arc, LazyLock},
        time::SystemTime,
    };

    pub use alloy::{
        primitives::{BlockNumber, BlockTimestamp, TxHash, U256},
        signers::local::PrivateKeySigner,
    };
    pub use anyhow::{Context as _, Result, bail};
    pub use clap::ValueEnum;
    pub use derive_more::{Display as DeriveMoreDisplay, FromStr};
    pub use moka::sync::Cache;
    pub use once_cell::sync::Lazy;
    pub use regex::Regex;
    pub use schemars::JsonSchema;
    pub use semver::{Version, VersionReq};
    pub use serde::{Deserialize, Serialize};
    pub use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
    pub use tokio::sync::{Mutex, Notify};

    pub use crate::define_wrapper_type;
}
