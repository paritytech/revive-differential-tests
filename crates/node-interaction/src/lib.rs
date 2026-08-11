//! This crate implements all node interactions.

pub mod config;
pub mod connector;
pub mod opcode_profile;
mod pool;
mod providers;
mod subxt_provider;
pub mod traits;

pub mod prelude {
    pub use crate::{config::*, connector::*, revive_metadata, traits::*};
}

pub(crate) mod internal_prelude {
    pub use crate::{
        pool::*, prelude::*, providers::*, revive_metadata::revive::calls::types::EthTransact,
        subxt_provider::*,
    };

    pub use std::{
        borrow::Cow,
        collections::{HashMap, hash_map::Entry},
        future::{Future, ready},
        ops::{ControlFlow, Deref},
        result::Result as StdResult,
        sync::{Arc, LazyLock, Mutex as StdMutex, atomic::AtomicUsize},
        time::{Duration, SystemTime},
    };

    pub use alloy::{
        consensus::{
            BlockHeader, Receipt, ReceiptEnvelope, Transaction, TxEip4844Variant, TxEnvelope,
            transaction::SignerRecoverable,
        },
        eips::{BlockId, Decodable2718, Encodable2718},
        network::{
            AnyNetwork, BlockResponse, Ethereum, EthereumWallet, Network, TransactionBuilder,
        },
        primitives::{Address, BlockHash, BlockNumber, TxHash, U256, address, keccak256},
        providers::{
            Identity, Provider, ProviderBuilder, RootProvider,
            ext::DebugApi,
            fillers::{
                ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, NonceManager,
                WalletFiller,
            },
        },
        rpc::{
            client::{BuiltInConnectionString, ClientBuilder},
            json_rpc::{RequestPacket, ResponsePacket},
            types::{
                Block as EvmBlock, TransactionReceipt, TransactionRequest,
                trace::geth::{GethDebugTracingCallOptions, GethDebugTracingOptions, GethTrace},
            },
        },
        transports::{
            BoxFuture, BoxTransport, Transport, TransportConnect, TransportError,
            TransportErrorKind, TransportFut, TransportResult,
        },
    };
    pub use anyhow::{Context as _, Error, Result, anyhow, bail};
    pub use bitflags::bitflags;
    pub use dashmap::DashMap;
    pub use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt, future::try_join_all};
    pub use pallet_revive::{EthTransactError, H256, Weight};
    pub use pallet_revive_types::runtime_api::{
        DryRunConfigV1, ExecutionTraceV1, ExecutionTracerConfigV1, GenericTransactionV1,
        ReceiptGasInfoV1, TraceV1, TracerTypeV1,
    };
    pub use parity_scale_codec::{Compact, Decode, Encode};
    pub use revive_common::EVMVersion;
    pub use revive_dt_common::futures::{
        AsyncHashMap, HeartbeatExt, StaticFuture, StaticStream, TimedExt,
        retry_future_with_exponential_backoff,
    };
    pub use serde::{Deserialize, Serialize};
    pub use serde_json;
    pub use sp_runtime::{
        OpaqueExtrinsic,
        generic::{Block as GenericBlock, DigestItem, Header as GenericHeader},
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
        ext::subxt_rpcs::{
            Error as RpcsError, methods::LegacyRpcMethods, utils::validate_url_is_secure,
        },
        tx::Payload,
    };
    pub use tokio::{
        spawn,
        sync::{
            Mutex, OwnedSemaphorePermit, RwLock, Semaphore,
            broadcast::{
                Receiver as BroadcastReceiver, Sender as BroadcastSender,
                channel as broadcast_channel,
            },
            mpsc,
        },
        time::{sleep, timeout},
    };
    pub use tokio_stream::wrappers::BroadcastStream;
    pub use tower::{Layer, Service};
    pub use tracing::{Instrument, debug, debug_span, error, info_span, trace, warn};
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
        path = "pallet_revive_types::runtime_api::types::traces::TraceV1",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::TraceV1>"
    ),
    substitute_type(
        path = "pallet_revive_types::runtime_api::types::tracer::TracerTypeV1",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::TracerTypeV1>"
    ),
    substitute_type(
        path = "pallet_revive_types::runtime_api::types::block::BlockV1",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::BlockV1>"
    ),
    substitute_type(
        path = "pallet_revive_types::runtime_api::types::transaction::GenericTransactionV1",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::GenericTransactionV1>"
    ),
    substitute_type(
        path = "pallet_revive_types::runtime_api::types::dry_run::DryRunConfigV1<M>",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::DryRunConfigV1<M>>"
    ),
    substitute_type(
        path = "pallet_revive::primitives::EthTransactError",
        with = "::subxt::utils::Static<::pallet_revive::EthTransactError>"
    ),
    substitute_type(
        path = "primitive_types::H160",
        with = "::subxt::utils::Static<::pallet_revive::H160>"
    ),
    substitute_type(
        path = "primitive_types::H256",
        with = "::subxt::utils::Static<::pallet_revive::H256>"
    ),
    substitute_type(
        path = "primitive_types::U256",
        with = "::subxt::utils::Static<::pallet_revive::U256>"
    ),
    substitute_type(
        path = "pallet_revive_types::runtime_api::types::receipt::ReceiptGasInfoV1",
        with = "::subxt::utils::Static<::pallet_revive_types::runtime_api::ReceiptGasInfoV1>"
    ),
    substitute_type(
        path = "sp_weights::weight_v2::Weight",
        with = "::subxt::utils::Static<::pallet_revive::Weight>"
    )
)]
pub mod revive_metadata {}
