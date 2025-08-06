use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, ChainId, U256};
use anyhow::Result;

/// A trait of the interface are required to implement to be used by the resolution logic that this
/// crate implements to go from string calldata and into the bytes calldata.
pub trait ResolverApi {
    /// Returns the ID of the chain that the node is on.
    fn chain_id(&self) -> impl Future<Output = Result<ChainId>>;

    // TODO: This is currently a u128 due to Kitchensink needing more than 64 bits for its gas limit
    // when we implement the changes to the gas we need to adjust this to be a u64.
    /// Returns the gas limit of the specified block.
    fn block_gas_limit(&self, number: BlockNumberOrTag) -> impl Future<Output = Result<u128>>;

    /// Returns the coinbase of the specified block.
    fn block_coinbase(&self, number: BlockNumberOrTag) -> impl Future<Output = Result<Address>>;

    /// Returns the difficulty of the specified block.
    fn block_difficulty(&self, number: BlockNumberOrTag) -> impl Future<Output = Result<U256>>;

    /// Returns the base fee of the specified block.
    fn block_base_fee(&self, number: BlockNumberOrTag) -> impl Future<Output = Result<u64>>;

    /// Returns the hash of the specified block.
    fn block_hash(&self, number: BlockNumberOrTag) -> impl Future<Output = Result<BlockHash>>;

    /// Returns the timestamp of the specified block,
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> impl Future<Output = Result<BlockTimestamp>>;

    /// Returns the number of the last block.
    fn last_block_number(&self) -> impl Future<Output = Result<BlockNumber>>;
}
