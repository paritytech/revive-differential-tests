//! This crate implements the reporting infrastructure for the differential testing tool.

mod aggregator;
mod common;
mod reporter_event;
mod runner_event;

pub mod prelude {
    pub use crate::aggregator::*;
    pub use crate::common::*;
    pub use crate::reporter_event::*;
    pub use crate::runner_event::*;
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_compiler::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_format::prelude::*;

    pub use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    pub use std::fs::OpenOptions;
    pub use std::path::{Path, PathBuf};
    pub use std::sync::Arc;
    pub use std::time::{SystemTime, UNIX_EPOCH};

    pub use alloy::hex;
    pub use alloy::json_abi::JsonAbi;
    pub use alloy::primitives::{Address, B256, BlockNumber, TxHash};
    pub use anyhow::Context as _;
    pub use anyhow::Result;
    pub use indexmap::IndexMap;
    pub use semver::Version;
    pub use serde::{Deserialize, Serialize};
    pub use serde_with::{DisplayFromStr, serde_as};
    pub use sha2::{Digest, Sha256};
    pub use tokio::sync::{
        broadcast::{self, Sender, channel},
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
        oneshot,
    };
    pub use tracing::debug;

    pub use revive_dt_common::define_wrapper_type;
}

pub use aggregator::*;
pub use common::*;
pub use reporter_event::*;
pub use runner_event::*;
