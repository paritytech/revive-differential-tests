//! This crate implements all node interactions.

pub mod config;
pub mod connector;
mod pool;
mod providers;
mod subxt_provider;
pub mod traits;

pub mod prelude {
    pub use crate::config::*;
    pub use crate::connector::*;
    pub use crate::revive_metadata;
    pub use crate::traits::*;
}

pub(crate) mod internal_prelude {
    pub use crate::pool::*;
    pub use crate::prelude::*;
    pub use crate::providers::*;
    pub use crate::revive_metadata::revive::calls::types::EthTransact;
    pub use crate::subxt_provider::*;

    pub use std::borrow::Cow;
    pub use std::collections::HashMap;
    pub use std::future::{Future, ready};
    pub use std::ops::{ControlFlow, Deref};
    pub use std::result::Result as StdResult;
    pub use std::sync::{Arc, LazyLock, Mutex as StdMutex, OnceLock, atomic::AtomicUsize};
    pub use std::time::{Duration, SystemTime};

    pub use alloy::consensus::BlockHeader;
    pub use alloy::eips::{BlockNumberOrTag, Encodable2718};
    pub use alloy::network::{
        AnyNetwork, BlockResponse, Ethereum, EthereumWallet, Network, TransactionBuilder,
    };
    pub use alloy::primitives::{Address, TxHash, U256, address, keccak256};
    pub use alloy::providers::{
        Identity, Provider, ProviderBuilder, RootProvider, SendableTx,
        ext::DebugApi,
        fillers::{
            CachedNonceManager, ChainIdFiller, FillProvider, GasFillable, GasFiller, JoinFill,
            NonceFiller, TxFiller, WalletFiller,
        },
    };
    pub use alloy::rpc::client::{BuiltInConnectionString, RpcClient};
    pub use alloy::rpc::json_rpc::{
        Id, RequestPacket, Response, ResponsePacket, SerializedRequest,
    };
    pub use alloy::rpc::types::trace::geth::{
        GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingCallOptions,
    };
    pub use alloy::rpc::types::trace::geth::{GethDebugTracingOptions, GethTrace};
    pub use alloy::rpc::types::{Block as EvmBlock, TransactionReceipt, TransactionRequest};
    pub use alloy::transports::{
        BoxFuture, BoxTransport, RpcError, Transport, TransportConnect, TransportError,
        TransportErrorKind, TransportFut, TransportResult,
    };
    pub use anyhow::{Context as _, Error, Result, anyhow, bail};
    pub use dashmap::DashMap;
    pub use futures::future::try_join_all;
    pub use futures::{FutureExt, StreamExt, TryFutureExt};
    pub use pallet_revive::{
        EthTransactError, H256, Weight,
        codec::{Decode, Encode},
        evm::TracerConfig,
    };
    pub use revive_common::EVMVersion;
    pub use revive_dt_common::futures::{
        AsyncHashMap, StaticFuture, StaticStream, retry_future_with_exponential_backoff,
    };
    pub use serde_json;
    pub use sp_runtime::{
        OpaqueExtrinsic,
        generic::{Block as GenericBlock, Header as GenericHeader},
        traits::BlakeTwo256,
    };
    pub use subxt::{
        OnlineClient, PolkadotConfig,
        backend::rpc::{
            RawRpcFuture, RawRpcSubscription, RawValue, RpcClient as SubxtRpcClient, RpcClientT,
            reconnecting_rpc_client,
        },
        blocks::Block as SubxtBlock,
        dynamic,
        ext::subxt_rpcs::{Error as RpcsError, utils::validate_url_is_secure},
        tx::Payload,
    };
    pub use tokio::sync::Mutex;
    pub use tokio::task::AbortHandle;
    pub use tokio::time::{interval, sleep, timeout};
    pub use tokio::{
        spawn,
        sync::{
            RwLock, Semaphore,
            broadcast::{
                Receiver as BroadcastReceiver, Sender as BroadcastSender,
                channel as broadcast_channel,
            },
        },
    };
    pub use tokio_stream::wrappers::BroadcastStream;
    pub use tower::{Layer, Service};
    pub use tracing::{Instrument, debug, debug_span, error, info, warn};
}

#[subxt::subxt(
    runtime_metadata_path = "../../assets/revive_metadata.scale",
    substitute_type(
        path = "sp_runtime::generic::block::Block<A, B, C, D, E>",
        with = "::subxt::utils::Static<::sp_runtime::generic::Block<
            ::sp_runtime::generic::Header<u32, ::sp_runtime::traits::BlakeTwo256>,
            ::sp_runtime::OpaqueExtrinsic
        >>"
    ),
    substitute_type(
        path = "pallet_revive::evm::api::debug_rpc_types::Trace",
        with = "::subxt::utils::Static<::pallet_revive::evm::Trace>"
    ),
    substitute_type(
        path = "pallet_revive::evm::api::debug_rpc_types::TracerType",
        with = "::subxt::utils::Static<::pallet_revive::evm::TracerType>"
    ),
    substitute_type(
        path = "pallet_revive::evm::api::rpc_types_gen::Block",
        with = "::subxt::utils::Static<pallet_revive::evm::Block>"
    ),
    substitute_type(
        path = "sp_weights::weight_v2::Weight",
        with = "::subxt::utils::Static<::pallet_revive::Weight>"
    )
)]
pub mod revive_metadata {}
