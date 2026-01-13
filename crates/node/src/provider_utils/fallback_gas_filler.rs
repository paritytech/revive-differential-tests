use alloy::{
    eips::BlockNumberOrTag,
    network::{Network, TransactionBuilder},
    providers::{
        Provider, SendableTx,
        ext::DebugApi,
        fillers::{GasFillable, GasFiller, TxFiller},
    },
    rpc::types::trace::geth::{
        GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingCallOptions,
        GethDebugTracingOptions,
    },
    transports::{RpcError, TransportResult},
};

/// An implementation of [`GasFiller`] with a fallback mechanism for reverting  transactions.
///
/// This struct provides a fallback mechanism for alloy's [`GasFiller`] which  kicks in when a
/// transaction's dry run fails due to it reverting allowing us to get gas estimates even for
/// failing transactions. In this codebase, this  is very important since the MatterLabs tests
/// expect some transactions in the test suite revert. Since we're expected to run a number of
/// assertions on these reverting transactions we must commit them to the ledger.
///
/// Therefore, this struct does the following:
///
/// 1. It first attempts to estimate the gas through the mechanism implemented in the [`GasFiller`].
/// 2. If it fails, then we perform a debug trace of the transaction to find  out how much gas the
///    transaction needs until it reverts.
/// 3. We fill in these values (either the success or failure case) into the transaction.
///
/// The fallback mechanism of this filler can be completely disabled if we don't want it to be used.
/// In that case, this gas filler will act in an identical  way to alloy's [`GasFiller`].
///
/// We then fill in these values into the transaction.
///
/// The previous implementation of this fallback gas filler relied on making use of default values
/// for the gas limit in order to be able to submit the reverting transactions to the network. But,
/// it introduced a number of issues that we weren't anticipating at the time when it was built.
#[derive(Clone, Copy, Debug)]
pub struct FallbackGasFiller {
    /// The inner [`GasFiller`] which we pass all of the calls to in the happy path.
    inner: GasFiller,

    /// A [`bool`] that controls if the fallback mechanism is enabled or not.
    enable_fallback_mechanism: bool,
}

impl FallbackGasFiller {
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
            enable_fallback_mechanism: true,
        }
    }

    pub fn with_fallback_mechanism(mut self, enable: bool) -> Self {
        self.enable_fallback_mechanism = enable;
        self
    }

    pub fn with_fallback_mechanism_enabled(self) -> Self {
        self.with_fallback_mechanism(true)
    }

    pub fn with_fallback_mechanism_disabled(self) -> Self {
        self.with_fallback_mechanism(false)
    }
}

impl<N> TxFiller<N> for FallbackGasFiller
where
    N: Network,
{
    type Fillable = <GasFiller as TxFiller<N>>::Fillable;

    fn status(
        &self,
        tx: &<N as Network>::TransactionRequest,
    ) -> alloy::providers::fillers::FillerControlFlow {
        TxFiller::<N>::status(&self.inner, tx)
    }

    fn fill_sync(&self, _: &mut SendableTx<N>) {}

    async fn prepare<P: Provider<N>>(
        &self,
        provider: &P,
        tx: &<N as Network>::TransactionRequest,
    ) -> TransportResult<Self::Fillable> {
        match (
            self.inner.prepare(provider, tx).await,
            self.enable_fallback_mechanism,
        ) {
            // Return the same thing if either this calls succeeds, or if the call falls and the
            // fallback mechanism is disabled.
            (rtn @ Ok(..), ..) | (rtn @ Err(..), false) => rtn,
            (Err(..), true) => {
                // Perform a trace of the transaction.
                let trace = provider
                    .debug_trace_call(
                        tx.clone(),
                        BlockNumberOrTag::Latest.into(),
                        GethDebugTracingCallOptions {
                            tracing_options: GethDebugTracingOptions {
                                tracer: Some(GethDebugTracerType::BuiltInTracer(
                                    GethDebugBuiltInTracerType::CallTracer,
                                )),
                                ..Default::default()
                            },
                            state_overrides: Default::default(),
                            block_overrides: Default::default(),
                            tx_index: Default::default(),
                        },
                    )
                    .await?
                    .try_into_call_frame()
                    .map_err(|err| {
                        RpcError::local_usage_str(
                            format!("Expected a callframe trace, but got: {err:?}").as_str(),
                        )
                    })?;

                let gas_used = u64::try_from(trace.gas_used).map_err(|_| {
                    RpcError::local_usage_str(
                        "Transaction trace returned a value of gas used that exceeds u64",
                    )
                })?;
                let gas_limit = gas_used.saturating_mul(2);

                if let Some(gas_price) = tx.gas_price() {
                    return Ok(GasFillable::Legacy {
                        gas_limit,
                        gas_price,
                    });
                }

                let estimate = if let (Some(max_fee_per_gas), Some(max_priority_fee_per_gas)) =
                    (tx.max_fee_per_gas(), tx.max_priority_fee_per_gas())
                {
                    alloy::eips::eip1559::Eip1559Estimation {
                        max_fee_per_gas,
                        max_priority_fee_per_gas,
                    }
                } else {
                    provider.estimate_eip1559_fees().await?
                };

                Ok(GasFillable::Eip1559 {
                    gas_limit,
                    estimate,
                })
            }
        }
    }

    async fn fill(
        &self,
        fillable: Self::Fillable,
        tx: SendableTx<N>,
    ) -> TransportResult<SendableTx<N>> {
        self.inner.fill(fillable, tx).await
    }
}

impl Default for FallbackGasFiller {
    fn default() -> Self {
        Self::new()
    }
}
