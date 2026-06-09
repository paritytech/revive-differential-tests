use crate::internal_prelude::*;

pub struct ResolutionContext<'a, A: LazyResolverApi> {
    pub metadata: Option<&'a Metadata>,
    pub pinned_block: Option<&'a BlockPair>,
    pub transaction_hash: Option<&'a TxHash>,
    pub node_connector: Option<Arc<NodeConnector>>,
    pub api: Option<&'a mut A>,
}

impl<'a, A: LazyResolverApi> ResolutionContext<'a, A> {
    pub fn pinned_block(&self) -> Option<&'a BlockPair> {
        self.pinned_block
    }

    pub fn transaction_hash(&self) -> Option<&'a TxHash> {
        self.transaction_hash
    }

    pub fn node_connector(&self) -> Option<Arc<NodeConnector>> {
        self.node_connector.clone()
    }

    pub async fn get_contract_address(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Option<anyhow::Result<Address>> {
        let api = self.api.as_mut()?;
        Some(
            api.get_contract_address(contract_ref)
                .await
                .context("Failed to get the contract address"),
        )
    }

    pub async fn get_variable(
        &mut self,
        variable: impl AsRef<str>,
    ) -> Option<anyhow::Result<U256>> {
        let api = self.api.as_mut()?;
        api.get_variable(variable).await
    }
}

#[allow(async_fn_in_trait)]
pub trait LazyResolverApi {
    async fn get_contract_address(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> anyhow::Result<Address>;

    async fn get_variable(&mut self, variable: impl AsRef<str>) -> Option<anyhow::Result<U256>>;
}
