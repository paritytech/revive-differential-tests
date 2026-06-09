//! The revive differential tests case format.

pub mod case;
pub mod corpus;
pub mod metadata;
pub mod steps;
pub mod traits;

pub mod prelude {
    pub use crate::case::*;
    pub use crate::corpus::*;
    pub use crate::metadata::*;
    pub use crate::steps::*;
    pub use crate::traits::*;
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::borrow::Cow;
    pub use std::cmp::Ordering;
    pub use std::collections::{BTreeMap, HashMap};
    pub use std::fmt::Display;
    pub use std::fs::File;
    pub use std::path::{Path, PathBuf};
    pub use std::str::FromStr;
    pub use std::sync::Arc;

    pub use alloy::hex::ToHexExt;
    pub use alloy::network::{Network, TransactionBuilder};
    pub use alloy::primitives::map::HashSet;
    pub use alloy::primitives::utils::parse_units;
    pub use alloy::primitives::{Address, Bytes, FixedBytes, TxHash, U256};
    pub use anyhow::Context as _;
    pub use derive_more::{Deref, Display, From, FromStr};
    pub use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt};
    pub use indexmap::IndexMap;
    pub use itertools::Itertools;
    pub use schemars::JsonSchema;
    pub use semver::VersionReq;
    pub use serde::{Deserialize, Serialize};
    pub use tracing::{Instrument, debug, error, info_span, instrument, warn};

    pub use revive_dt_common::macros::define_wrapper_type;

    pub use revive_common::EVMVersion;
}
