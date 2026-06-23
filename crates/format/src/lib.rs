//! The revive differential tests case format.

pub mod case;
pub mod corpus;
pub mod metadata;
pub mod steps;
pub mod traits;

pub mod prelude {
    pub use crate::{case::*, corpus::*, metadata::*, steps::*, traits::*};
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::{
        borrow::Cow,
        cmp::Ordering,
        collections::{BTreeMap, HashMap},
        fmt::Display,
        fs::File,
        path::{Path, PathBuf},
        str::FromStr,
        sync::Arc,
    };

    pub use alloy::{
        hex::ToHexExt,
        network::{Network, TransactionBuilder},
        primitives::{Address, Bytes, FixedBytes, TxHash, U256, map::HashSet, utils::parse_units},
    };
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
