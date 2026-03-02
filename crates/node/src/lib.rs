//! This crate implements the testing nodes.

use crate::internal_prelude::*;

pub mod constants;
pub mod helpers;
pub mod node_implementations;
pub mod provider_utils;

pub mod prelude {
    pub use crate::Node;
    pub use crate::constants::*;
    pub use crate::helpers::*;
    pub use crate::node_implementations::geth::GethNode;
    pub use crate::node_implementations::lighthouse_geth::LighthouseGethNode;
    pub use crate::node_implementations::polkadot_omni_node::PolkadotOmnichainNode;
    pub use crate::node_implementations::substrate::SubstrateNode;
    pub use crate::node_implementations::zombienet::ZombienetNode;
    pub use crate::provider_utils::*;
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
    pub use revive_dt_config::prelude::*;
    pub use revive_dt_node_interaction::prelude::*;

    pub use std::collections::{BTreeMap, HashSet};
    pub use std::fs::{File, OpenOptions, create_dir_all, remove_dir_all};
    pub use std::io::{BufRead, BufReader, Read, Write};
    pub use std::path::{Path, PathBuf};
    pub use std::process::{Child, Command, Stdio};
    pub use std::sync::atomic::{AtomicU32, Ordering};
    pub use std::sync::{Arc, LazyLock, Mutex};
    pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub use alloy::{
        consensus::BlockHeader,
        eips::BlockNumberOrTag,
        genesis::{Genesis, GenesisAccount},
        network::{
            AnyNetwork, BlockResponse, Ethereum, EthereumWallet, Network, NetworkWallet,
            TransactionBuilder, TransactionBuilder4844,
        },
        primitives::{Address, ChainId, U256, address},
        providers::{
            DynProvider, Identity, Provider, ProviderBuilder, RootProvider, SendableTx,
            ext::DebugApi,
            fillers::{
                CachedNonceManager, ChainIdFiller, FillProvider, GasFillable, GasFiller,
                JoinFill, NonceFiller, TxFiller, WalletFiller,
            },
        },
        rpc::{
            client::ClientBuilder,
            json_rpc::{RequestPacket, ResponsePacket},
            types::{
                TransactionRequest,
                trace::geth::{
                    GethDebugBuiltInTracerType, GethDebugTracerType,
                    GethDebugTracingCallOptions, GethDebugTracingOptions,
                },
            },
        },
        transports::{BoxFuture, RpcError, TransportError, TransportErrorKind, TransportFut, TransportResult},
    };
    pub use anyhow::Context as _;
    pub use anyhow::{Result, bail};
    pub use futures::StreamExt;
    pub use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub use serde_json::{self, Value, json};
    pub use serde_with::serde_as;
    pub use serde_yaml_ng;
    pub use sp_core::crypto::Ss58Codec;
    pub use sp_runtime::AccountId32;
    pub use subxt::{OnlineClient, SubstrateConfig};
    pub use tokio::sync::{OnceCell, Semaphore};
    pub use tokio::time::{interval, timeout};
    pub use toml;
    pub use tower::{Layer, Service};
    pub use tracing::{error, info, instrument, trace};
    pub use zombienet_sdk::{LocalFileSystem, NetworkConfig, NetworkConfigExt};

    pub use revive_common::EVMVersion;
}

/// An abstract interface for testing nodes.
pub trait Node: NodeApi {
    /// Spawns a node configured according to the genesis json.
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<()>;

    /// Prune the node instance and related data.
    ///
    /// Blocking until it's completely stopped.
    fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Returns the node version.
    fn version(&self) -> anyhow::Result<String>;
}
