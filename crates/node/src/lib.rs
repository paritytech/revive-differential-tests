//! This crate implements the testing nodes.

pub mod constants;
pub mod helpers;
pub mod node_implementations;
pub mod node_process;

pub mod prelude {
    #[cfg(unix)]
    pub use crate::node_implementations::zombienet::ZombienetNode;
    pub use crate::{
        constants::*,
        helpers::*,
        node_implementations::{
            geth::GethNode, lighthouse_geth::LighthouseGethNode,
            polkadot_omni_node::PolkadotOmnichainNode, revive_dev_node::ReviveDevNode,
        },
        node_process::*,
    };
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::{
        borrow::Cow,
        collections::{BTreeMap, HashMap, hash_map::Entry as HashMapEntry},
        ffi::{OsStr, OsString},
        fs::{File, OpenOptions, create_dir_all, remove_dir_all},
        io::{BufRead, BufReader, BufWriter, Read, Write},
        net::TcpListener,
        ops::ControlFlow,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{
            Arc, LazyLock, Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    pub use alloy::{
        consensus::BlockHeader,
        genesis::{Genesis, GenesisAccount},
        network::{Ethereum, EthereumWallet, NetworkWallet, TransactionBuilder},
        primitives::{Address, ChainId, U256},
        providers::{Provider, fillers::TxFiller},
    };
    pub use anyhow::{Context as _, Result, bail};
    pub use futures::{FutureExt, StreamExt};
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
