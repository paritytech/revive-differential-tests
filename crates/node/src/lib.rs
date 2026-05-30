//! This crate implements the testing nodes.

use crate::internal_prelude::*;

pub mod constants;
pub mod helpers;
pub mod node_implementations;
pub mod node_process;
pub mod provider_utils;

pub mod prelude {
    pub use crate::Node;
    pub use crate::constants::*;
    pub use crate::helpers::*;
    pub use crate::node_implementations::geth::GethNode;
    pub use crate::node_implementations::lighthouse_geth::LighthouseGethNode;
    pub use crate::node_implementations::polkadot_omni_node::PolkadotOmnichainNode;
    pub use crate::node_implementations::revive_dev_node::ReviveDevNode;
    #[cfg(unix)]
    pub use crate::node_implementations::zombienet::ZombienetNode;
    pub use crate::node_process::*;
    pub use crate::provider_utils::*;
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use revive_dt_common::prelude::*;
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
    pub use std::sync::atomic::{AtomicU32, Ordering};
    pub use std::sync::{Arc, LazyLock, Mutex as StdMutex};
    pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub use alloy::{
        consensus::BlockHeader,
        eips::{BlockNumberOrTag, Encodable2718},
        genesis::{Genesis, GenesisAccount},
        network::{
            AnyNetwork, BlockResponse, Ethereum, EthereumWallet, Network, NetworkWallet,
            TransactionBuilder, TransactionBuilder4844,
        },
        primitives::{Address, ChainId, TxHash, U256},
        providers::{
            DynProvider, Identity, Provider, ProviderBuilder, RootProvider, SendableTx,
            ext::DebugApi,
            fillers::{
                CachedNonceManager, ChainIdFiller, FillProvider, GasFillable, GasFiller, JoinFill,
                NonceFiller, TxFiller, WalletFiller,
            },
        },
        rpc::{
            client::ClientBuilder,
            json_rpc::{Id, RequestPacket, Response, ResponsePacket, SerializedRequest},
            types::{
                TransactionRequest,
                trace::geth::{
                    GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingCallOptions,
                    GethDebugTracingOptions,
                },
            },
        },
        transports::{
            BoxFuture, RpcError, TransportError, TransportErrorKind, TransportFut, TransportResult,
        },
    };
    pub use anyhow::{Context as _, Result, bail};
    pub use futures::FutureExt;
    pub use futures::StreamExt;
    pub use serde_json::{self, Value, json};
    pub use sp_core::crypto::Ss58Codec;
    pub use sp_runtime::AccountId32;
    pub use subxt::{OnlineClient, PolkadotConfig, tx::TxStatus};
    pub use tokio::{
        runtime::Runtime,
        sync::{OnceCell, Semaphore},
        task::AbortHandle,
        time::{interval, sleep, timeout},
    };
    pub use toml;
    pub use tower::{Layer, Service};
    #[allow(unused_imports)]
    pub use tracing::{
        Instrument, Span, debug, debug_span, error, error_span, info, info_span, instrument, trace,
        trace_span, warn, warn_span,
    };
    #[cfg(unix)]
    pub use zombienet_sdk::{
        LocalFileSystem, Network as ZombienetNetwork, NetworkConfig, NetworkConfigExt,
    };

    pub use revive_common::EVMVersion;
}

/// An abstract interface for testing nodes.
pub trait Node: NodeApi {
    /// Spawns a node configured according to the genesis json.
    ///
    /// Blocking until it's ready to accept transactions.
    fn spawn(&mut self, genesis: Genesis) -> Result<()>;
}
