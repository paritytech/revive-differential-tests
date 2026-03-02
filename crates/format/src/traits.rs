use std::collections::HashMap;

use alloy::eips::BlockNumberOrTag;
use alloy::json_abi::JsonAbi;
use alloy::primitives::TxHash;
use alloy::primitives::{Address, BlockNumber, U256};

use crate::metadata::{ContractIdent, ContractInstance};

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
        self.deployed_contracts
            .and_then(|deployed_contracts| deployed_contracts.get(instance))
    }

    pub fn deployed_contract_address(&self, instance: &ContractInstance) -> Option<&Address> {
        self.deployed_contract(instance).map(|(_, a, _)| a)
    }

    pub fn deployed_contract_abi(&self, instance: &ContractInstance) -> Option<&JsonAbi> {
        self.deployed_contract(instance).map(|(_, _, a)| a)
    }

    pub fn variable(&self, name: impl AsRef<str>) -> Option<&U256> {
        self.variables
            .and_then(|variables| variables.get(name.as_ref()))
    }

    pub fn tip_block_number(&self) -> Option<&'a BlockNumber> {
        self.block_number
    }

    pub fn transaction_hash(&self) -> Option<&'a TxHash> {
        self.transaction_hash
    }
}
