use crate::internal_prelude::*;

/// The deployed contract information keyed by contract instance name.
pub type DeployedContracts = HashMap<ContractInstance, Arc<(ContractIdent, Address, JsonAbi)>>;

#[derive(Clone, Copy, Default)]
/// Contextual information required by the code that's performing the resolution.
pub struct ResolutionContext<'a> {
    /// When provided this metadata file will be used for for resolutions.
    metadata: Option<&'a Metadata>,

    /// When provided the contracts provided here will be used for resolutions.
    deployed_contracts: Option<&'a DeployedContracts>,

    /// When provided the variables in here will be used for performing resolutions.
    variables: Option<&'a HashMap<String, U256>>,

    /// When provided resolution will use this exact block instead of querying latest.
    pinned_block: Option<&'a BlockPair>,

    /// When provided resolution will use this block instead of the latest block.
    pinned_block_number: Option<&'a BlockNumber>,

    /// When provided resolution will use this transaction hash.
    transaction_hash: Option<&'a TxHash>,

    /// When provided resolution will use this connector for operations that require node access.
    node_connector: Option<&'a NodeConnector>,
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
        deployed_contracts: impl Into<Option<&'a DeployedContracts>>,
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

    pub fn with_pinned_block(mut self, pinned_block: impl Into<Option<&'a BlockPair>>) -> Self {
        self.pinned_block = pinned_block.into();
        self
    }

    pub fn with_pinned_block_number(
        mut self,
        pinned_block_number: impl Into<Option<&'a BlockNumber>>,
    ) -> Self {
        self.pinned_block_number = pinned_block_number.into();
        self
    }

    pub fn with_transaction_hash(
        mut self,
        transaction_hash: impl Into<Option<&'a TxHash>>,
    ) -> Self {
        self.transaction_hash = transaction_hash.into();
        self
    }

    pub fn with_node_connector(
        mut self,
        node_connector: impl Into<Option<&'a NodeConnector>>,
    ) -> Self {
        self.node_connector = node_connector.into();
        self
    }

    pub async fn deployed_contract<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> Option<&(ContractIdent, Address, JsonAbi)> {
        let instance = self.contract_instance(instance).await?;
        self.deployed_contracts
            .and_then(|deployed_contracts| deployed_contracts.get(&instance))
            .map(|arc| arc.as_ref())
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

    pub fn pinned_block(&self) -> Option<&'a BlockPair> {
        self.pinned_block
    }

    pub fn pinned_block_number(&self) -> Option<&'a BlockNumber> {
        self.pinned_block_number
    }

    pub fn transaction_hash(&self) -> Option<&'a TxHash> {
        self.transaction_hash
    }

    pub fn node_connector(&self) -> Option<&'a NodeConnector> {
        self.node_connector
    }

    pub fn contract_instance<'b>(
        &self,
        instance: impl Into<ContractInstanceOrReference<'b>>,
    ) -> impl Future<Output = Option<ContractInstance>> {
        Box::pin(async move {
            let instance = instance.into();
            match instance {
                ContractInstanceOrReference::Reference(reference) => {
                    let metadata = self.metadata.as_ref()?;
                    let contracts = metadata.contracts.as_ref()?;

                    let CalldataToken::Item(resolved) = reference
                        .into_owned()
                        .into_inner()
                        .resolve(*self)
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
