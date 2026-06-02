use crate::internal_prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeConnectorConfiguration {
    pub eth_provider_configuration: Option<EthProviderConfiguration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EthProviderConfiguration {
    pub gas_filler_configuration: Option<GasFillerConfiguration>,
    pub retry_configuration: Option<RetryConfiguration>,
    pub batching_configuration: Option<BatchingConfiguration>,
    pub concurrency_configuration: Option<ConcurrencyConfiguration>,
    pub global_concurrency_configuration: Option<ConcurrencyConfiguration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GasFillerConfiguration {
    DisableTracingFallback,
    EnableTracingFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RetryConfiguration {
    Disabled,
    Enabled {
        polling_duration: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatchingConfiguration {
    Disabled,
    Enabled {
        max_batch_size: usize,
        batching_duration: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConcurrencyConfiguration {
    Disabled,
    SemaphoreBasedLimiter { permits: usize },
}
