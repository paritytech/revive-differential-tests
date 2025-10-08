use std::{collections::HashMap, pin::Pin};

use alloy::{
    eips::BlockNumberOrTag,
    json_abi::JsonAbi,
    primitives::{Address, BlockHash, BlockNumber, BlockTimestamp, ChainId, TxHash, U256},
};
use anyhow::Result;

use crate::metadata::{ContractIdent, ContractInstance};

/// A trait of the interface are required to implement to be used by the resolution logic that this
/// crate implements to go from string calldata and into the bytes calldata.
pub trait ResolverApi {
    /// Returns the ID of the chain that the node is on.
    fn chain_id(&self) -> Pin<Box<dyn Future<Output = Result<ChainId>> + '_>>;

    /// Returns the gas price for the specified transaction.
    fn transaction_gas_price(
        &self,
        tx_hash: TxHash,
    ) -> Pin<Box<dyn Future<Output = Result<u128>> + '_>>;

    // TODO: This is currently a u128 due to substrate needing more than 64 bits for its gas limit
    // when we implement the changes to the gas we need to adjust this to be a u64.
    /// Returns the gas limit of the specified block.
    fn block_gas_limit(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<u128>> + '_>>;

    /// Returns the coinbase of the specified block.
    fn block_coinbase(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<Address>> + '_>>;

    /// Returns the difficulty of the specified block.
    fn block_difficulty(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<U256>> + '_>>;

    /// Returns the base fee of the specified block.
    fn block_base_fee(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<u64>> + '_>>;

    /// Returns the hash of the specified block.
    fn block_hash(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<BlockHash>> + '_>>;

    /// Returns the timestamp of the specified block,
    fn block_timestamp(
        &self,
        number: BlockNumberOrTag,
    ) -> Pin<Box<dyn Future<Output = Result<BlockTimestamp>> + '_>>;

    /// Returns the number of the last block.
    fn last_block_number(&self) -> Pin<Box<dyn Future<Output = Result<BlockNumber>> + '_>>;
}

#[derive(Clone, Copy, Debug, Default)]
/// Contextual information required by the code that's performing the resolution.
pub struct ResolutionContext<'a> {
    /// When provided the contracts provided here will be used for resolutions.
    deployed_contracts: Option<&'a HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,

    /// When provided the variables in here will be used for performing resolutions.
    variables: Option<&'a HashMap<String, U256>>,

    /// When provided this block number will be treated as the tip of the chain.
    block_number: Option<&'a BlockNumber>,

    /// When provided the resolver will use this transaction hash for all of its resolutions.
    transaction_hash: Option<&'a TxHash>,
}

impl<'a> ResolutionContext<'a> {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn new_from_parts(
        deployed_contracts: impl Into<
            Option<&'a HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,
        >,
        variables: impl Into<Option<&'a HashMap<String, U256>>>,
        block_number: impl Into<Option<&'a BlockNumber>>,
        transaction_hash: impl Into<Option<&'a TxHash>>,
    ) -> Self {
        Self {
            deployed_contracts: deployed_contracts.into(),
            variables: variables.into(),
            block_number: block_number.into(),
            transaction_hash: transaction_hash.into(),
        }
    }

    pub fn with_deployed_contracts(
        mut self,
        deployed_contracts: impl Into<
            Option<&'a HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,
        >,
    ) -> Self {
        self.deployed_contracts = deployed_contracts.into();
        self
    }

    pub fn with_variables(
        mut self,
        variables: impl Into<Option<&'a HashMap<String, U256>>>,
    ) -> Self {
        self.variables = variables.into();
        self
    }

    pub fn with_block_number(mut self, block_number: impl Into<Option<&'a BlockNumber>>) -> Self {
        self.block_number = block_number.into();
        self
    }

    pub fn with_transaction_hash(
        mut self,
        transaction_hash: impl Into<Option<&'a TxHash>>,
    ) -> Self {
        self.transaction_hash = transaction_hash.into();
        self
    }

    pub fn resolve_block_number(&self, number: BlockNumberOrTag) -> BlockNumberOrTag {
        match self.block_number {
            Some(block_number) => match number {
                BlockNumberOrTag::Latest => BlockNumberOrTag::Number(*block_number),
                n @ (BlockNumberOrTag::Finalized
                | BlockNumberOrTag::Safe
                | BlockNumberOrTag::Earliest
                | BlockNumberOrTag::Pending
                | BlockNumberOrTag::Number(_)) => n,
            },
            None => number,
        }
    }

    pub fn deployed_contract(
        &self,
        instance: &ContractInstance,
    ) -> Option<&(ContractIdent, Address, JsonAbi)> {
        self.deployed_contracts.and_then(|deployed_contracts| deployed_contracts.get(instance))
    }

    pub fn deployed_contract_address(&self, instance: &ContractInstance) -> Option<&Address> {
        self.deployed_contract(instance).map(|(_, a, _)| a)
    }

    pub fn deployed_contract_abi(&self, instance: &ContractInstance) -> Option<&JsonAbi> {
        self.deployed_contract(instance).map(|(_, _, a)| a)
    }

    pub fn variable(&self, name: impl AsRef<str>) -> Option<&U256> {
        self.variables.and_then(|variables| variables.get(name.as_ref()))
    }

    pub fn tip_block_number(&self) -> Option<&'a BlockNumber> {
        self.block_number
    }

    pub fn transaction_hash(&self) -> Option<&'a TxHash> {
        self.transaction_hash
    }
}
