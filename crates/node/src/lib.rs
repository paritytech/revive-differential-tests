//! This crate implements the testing nodes.

pub mod constants;
pub mod helpers;
pub mod node_implementations;
pub mod node_process;

pub mod prelude {
    pub use crate::constants::*;
    pub use crate::helpers::*;
    pub use crate::node_implementations::geth::GethNode;
    pub use crate::node_implementations::lighthouse_geth::LighthouseGethNode;
    pub use crate::node_implementations::polkadot_omni_node::PolkadotOmnichainNode;
    pub use crate::node_implementations::revive_dev_node::ReviveDevNode;
    #[cfg(unix)]
    pub use crate::node_implementations::zombienet::ZombienetNode;
    pub use crate::node_process::*;
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::borrow::Cow;
    pub use std::collections::{BTreeMap, HashMap, hash_map::Entry as HashMapEntry};
    pub use std::ffi::{OsStr, OsString};
    pub use std::fs::{File, OpenOptions, create_dir_all, remove_dir_all};
    pub use std::io::{BufRead, BufReader, BufWriter, Read, Write};
    pub use std::net::TcpListener;
    pub use std::ops::ControlFlow;
    pub use std::path::{Path, PathBuf};
    pub use std::process::{Child, Command, Stdio};
    pub use std::sync::atomic::{AtomicUsize, Ordering};
    pub use std::sync::{Arc, LazyLock, Mutex as StdMutex};
    pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub use alloy::{
        consensus::BlockHeader,
        genesis::{Genesis, GenesisAccount},
        network::{Ethereum, EthereumWallet, NetworkWallet, TransactionBuilder},
        primitives::{Address, ChainId, U256},
        providers::{Provider, fillers::TxFiller},
    };
    pub use anyhow::{Context as _, Result, bail};
    pub use futures::FutureExt;
    pub use futures::StreamExt;
    pub use serde_json::{self, Value, json};
    pub use sp_core::crypto::Ss58Codec;
    pub use sp_runtime::AccountId32;
    pub use subxt::{OnlineClient, PolkadotConfig};
    pub use tokio::runtime::Runtime;
    pub use toml;
    pub use tracing::error;
    #[cfg(unix)]
    pub use zombienet_sdk::{
        LocalFileSystem, Network as ZombienetNetwork, NetworkConfig, NetworkConfigExt,
    };

    pub use revive_common::EVMVersion;
}
