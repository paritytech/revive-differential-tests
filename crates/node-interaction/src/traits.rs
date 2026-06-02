use crate::internal_prelude::*;

pub trait NodeConfiguration {
    fn configurations(&self) -> NodeConnectorConfiguration;
    fn evm_version(&self) -> EVMVersion;
    fn eth_provider_url(&self) -> NodeUrlCollection<'_>;
    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>>;
}

pub struct NodeUrlCollection<'a> {
    pub ipc: Option<Cow<'a, str>>,
    pub http: Option<Cow<'a, str>>,
    pub ws: Option<Cow<'a, str>>,
}
