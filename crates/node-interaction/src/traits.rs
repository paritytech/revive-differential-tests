use crate::internal_prelude::*;

pub trait NodeConfiguration {
    fn configurations(&self) -> NodeConnectorConfiguration {
        Default::default()
    }

    fn id(&self) -> usize;
    fn evm_version(&self) -> EVMVersion;
    fn eth_provider_url(&self) -> NodeUrlCollection<'_>;
    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>>;
}

#[derive(Default)]
pub struct NodeUrlCollection<'a> {
    pub ipc: Option<Cow<'a, str>>,
    pub http: Option<Cow<'a, str>>,
    pub ws: Option<Cow<'a, str>>,
}

impl<'a> NodeUrlCollection<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ipc_url(mut self, url: impl Into<Cow<'a, str>>) -> Self {
        self.ipc = Some(url.into());
        self
    }

    pub fn with_http_url(mut self, url: impl Into<Cow<'a, str>>) -> Self {
        self.http = Some(url.into());
        self
    }

    pub fn with_ws_url(mut self, url: impl Into<Cow<'a, str>>) -> Self {
        self.ws = Some(url.into());
        self
    }
}
