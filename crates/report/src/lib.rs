//! This crate implements the reporting infrastructure for the differential testing tool.

mod aggregator;
mod common;
mod reporter_event;
mod runner_event;

pub mod prelude {
    pub use crate::{aggregator::*, common::*, reporter_event::*, runner_event::*};
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_compiler::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_format::prelude::*;

    pub use std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet},
        fs::OpenOptions,
        io::BufWriter,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    pub use alloy::{
        hex,
        json_abi::JsonAbi,
        primitives::{Address, B256, BlockNumber, TxHash, keccak256},
    };
    pub use anyhow::{Context as _, Result};
    pub use indexmap::IndexMap;
    pub use semver::Version;
    pub use serde::{Deserialize, Serialize};
    pub use serde_with::{DisplayFromStr, serde_as};
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
