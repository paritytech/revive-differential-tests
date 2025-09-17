//! This crate implements all node interactions.

use std::pin::Pin;

use alloy::primitives::{Address, StorageKey, TxHash, U256};
use alloy::rpc::types::trace::geth::{DiffMode, GethDebugTracingOptions, GethTrace};
use alloy::rpc::types::{EIP1186AccountProofResponse, TransactionReceipt, TransactionRequest};
use anyhow::Result;
use revive_dt_format::traits::ResolverApi;

/// An interface for all interactions with Ethereum compatible nodes.
#[allow(clippy::type_complexity)]
pub trait EthereumNode {
    /// Execute the [TransactionRequest] and return a [TransactionReceipt].
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TransactionReceipt>> + '_>>;

    /// Trace the transaction in the [TransactionReceipt] and return a [GethTrace].
    fn trace_transaction(
        &self,
        tx_hash: TxHash,
        trace_options: GethDebugTracingOptions,
    ) -> Pin<Box<dyn Future<Output = Result<GethTrace>> + '_>>;

    /// Returns the state diff of the transaction hash in the [TransactionReceipt].
    fn state_diff(&self, tx_hash: TxHash) -> Pin<Box<dyn Future<Output = Result<DiffMode>> + '_>>;

    /// Returns the balance of the provided [`Address`] back.
    fn balance_of(&self, address: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + '_>>;

    /// Returns the latest storage proof of the provided [`Address`]
    fn latest_state_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> Pin<Box<dyn Future<Output = Result<EIP1186AccountProofResponse>> + '_>>;

    /// Returns the resolver that is to use with this ethereum node.
    fn resolver(&self) -> Pin<Box<dyn Future<Output = Result<Box<dyn ResolverApi + '_>>> + '_>>;
}
