use crate::internal_prelude::*;

macro_rules! resolve {
    ($a: expr, $b: expr $(,)?) => {
        match ($a, $b) {
            (Some(a), Some(b)) => Some(a.resolve(b)),
            (Some(v), None) | (None, Some(v)) => Some(v),
            (None, None) => None,
        }
    };
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct NodeConnectorConfiguration {
    pub hooks: Option<NodeConnectorHooks>,
    pub behaviors: Option<NodeConnectorBehaviors>,
    pub substrate_provider_configuration: Option<SubstrateProviderConfiguration>,
    pub eth_provider_configuration: Option<EthProviderConfiguration>,
    pub block_provisioning_behavior: Option<BlockProvisioningBehavior>,
}

impl NodeConnectorConfiguration {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            hooks: resolve!(self.hooks, other.hooks),
            behaviors: resolve!(self.behaviors, other.behaviors),
            substrate_provider_configuration: resolve!(
                self.substrate_provider_configuration,
                other.substrate_provider_configuration
            ),
            eth_provider_configuration: resolve!(
                self.eth_provider_configuration,
                other.eth_provider_configuration
            ),
            block_provisioning_behavior: resolve!(
                self.block_provisioning_behavior,
                other.block_provisioning_behavior
            ),
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct NodeConnectorBehaviors {
    pub submission_behavior: Option<SubmissionBehavior>,
}

impl NodeConnectorBehaviors {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            submission_behavior: self.submission_behavior.or(other.submission_behavior),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SubmissionBehavior {
    UseDefaultForPlatform,
    UseEthRpc,
    UseSubstrateRpc,
    UseSubstrateRpcAndAwaitInclusion,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct NodeConnectorHooks {
    pub pre_submission_hook: Option<PreSubmissionHook>,
}

impl NodeConnectorHooks {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            pre_submission_hook: self.pre_submission_hook.or(other.pre_submission_hook),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PreSubmissionHook {
    Disabled,
    MaxGasPrice,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct SubstrateProviderConfiguration {
    pub submission_concurrency_configuration: Option<ProviderConcurrencyConfiguration>,
    pub allowed_estimation_methods: Option<AllowedEstimationMethods>,
}

impl SubstrateProviderConfiguration {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            submission_concurrency_configuration: self
                .submission_concurrency_configuration
                .or(other.submission_concurrency_configuration),
            allowed_estimation_methods: self
                .allowed_estimation_methods
                .or(other.allowed_estimation_methods),
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    pub struct AllowedEstimationMethods: u8 {
        const ESTIMATE_GAS = 1 << 0;
        const ETH_TRANSACT_WITH_CONFIG = 1 << 1;
        const ETH_TRANSACT = 1 << 2;
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct EthProviderConfiguration {
    pub retry_configuration: Option<RetryConfiguration>,
    pub concurrency_configuration: Option<ProviderConcurrencyConfiguration>,
    pub global_concurrency_configuration: Option<ProviderConcurrencyConfiguration>,
}

impl EthProviderConfiguration {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            retry_configuration: self.retry_configuration.or(other.retry_configuration),
            concurrency_configuration: self
                .concurrency_configuration
                .or(other.concurrency_configuration),
            global_concurrency_configuration: self
                .global_concurrency_configuration
                .or(other.global_concurrency_configuration),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RetryConfiguration {
    Disabled,
    Enabled {
        polling_duration: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProviderConcurrencyConfiguration {
    Disabled,
    SemaphoreBasedLimiter { permits: usize },
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct BlockProvisioningBehavior {
    pub subscription_kind: Option<BlockProvisioningSubscriptionKind>,
}

impl BlockProvisioningBehavior {
    pub fn resolve(self, other: Self) -> Self {
        Self {
            subscription_kind: self.subscription_kind.or(other.subscription_kind),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BlockProvisioningSubscriptionKind {
    BestBlocks,
    FinalizedBlocks,
}
