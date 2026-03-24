use crate::internal_prelude::*;

#[derive(Clone, Copy, Default)]
/// Contextual information required by the code that's performing the resolution.
pub struct ResolutionContext<'a> {
    /// When provided this metadata file will be used for for resolutions.
    metadata: Option<&'a Metadata>,

    /// When provided the contracts provided here will be used for resolutions.
    deployed_contracts: Option<&'a HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,

    /// When provided the variables in here will be used for performing resolutions.
    variables: Option<&'a HashMap<String, U256>>,

    /// When provided this block number will be treated as the tip of the chain.
    block_number: Option<&'a BlockNumber>,

    /// When provided the resolver will use this transaction hash for all of its resolutions.
    transaction_hash: Option<&'a TxHash>,

    /// When provided the resolver will use this node for any operations which require node access.
    node_api: Option<&'a dyn NodeApi>,
}

impl<'a> ResolutionContext<'a> {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_metadata(mut self, metadata: impl Into<Option<&'a Metadata>>) -> Self {
        self.metadata = metadata.into();
        self
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

    pub fn with_node_api(mut self, node_api: impl Into<Option<&'a dyn NodeApi>>) -> Self {
        self.node_api = node_api.into();
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

    pub async fn deployed_contract<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> Option<&(ContractIdent, Address, JsonAbi)> {
        let instance = self.contract_instance(instance).await?;
        self.deployed_contracts
            .and_then(|deployed_contracts| deployed_contracts.get(&instance))
    }

    pub async fn deployed_contract_address<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> Option<&Address> {
        self.deployed_contract(instance).await.map(|(_, a, _)| a)
    }

    pub async fn deployed_contract_abi<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> Option<&JsonAbi> {
        self.deployed_contract(instance).await.map(|(_, _, a)| a)
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

    pub fn contract_instance<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> impl Future<Output = Option<ContractInstance>> {
        Box::pin(async move {
            let instance = instance.into();
            match instance {
                ContractInstanceOrReference::Reference(reference) => {
                    let node_api = self.node_api.as_ref()?;
                    let metadata = self.metadata.as_ref()?;
                    let contracts = metadata.contracts.as_ref()?;

                    let CalldataToken::Item(resolved) = reference
                        .into_owned()
                        .into_inner()
                        .resolve(*node_api, *self)
                        .await
                        .ok()?
                    else {
                        return None;
                    };

                    contracts
                        .get_index(resolved.try_into().ok()?)
                        .map(|(instance, _)| instance.clone())
                }
                ContractInstanceOrReference::Instance(instance) => Some(instance.into_owned()),
            }
        })
    }
}
