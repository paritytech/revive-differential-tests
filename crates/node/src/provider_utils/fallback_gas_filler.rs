use alloy::{
    network::{Network, TransactionBuilder},
    providers::{
        Provider, SendableTx,
        fillers::{GasFiller, TxFiller},
    },
    transports::TransportResult,
};

// Percentage padding applied to estimated gas (e.g. 120 = 20% padding)
const GAS_ESTIMATE_PADDING_NUMERATOR: u64 = 120;
const GAS_ESTIMATE_PADDING_DENOMINATOR: u64 = 100;

#[derive(Clone, Debug)]
pub struct FallbackGasFiller {
    inner: GasFiller,
    default_gas_limit: u64,
    default_max_fee_per_gas: u128,
    default_priority_fee: u128,
}

impl FallbackGasFiller {
    pub fn new(
        default_gas_limit: u64,
        default_max_fee_per_gas: u128,
        default_priority_fee: u128,
    ) -> Self {
        Self {
            inner: GasFiller,
            default_gas_limit,
            default_max_fee_per_gas,
            default_priority_fee,
        }
    }
}

impl Default for FallbackGasFiller {
    fn default() -> Self {
        FallbackGasFiller::new(25_000_000, 1_000_000_000, 1_000_000_000)
    }
}

impl<N> TxFiller<N> for FallbackGasFiller
where
    N: Network,
{
    type Fillable = Option<<GasFiller as TxFiller<N>>::Fillable>;

    fn status(
        &self,
        tx: &<N as Network>::TransactionRequest,
    ) -> alloy::providers::fillers::FillerControlFlow {
        <GasFiller as TxFiller<N>>::status(&self.inner, tx)
    }

    fn fill_sync(&self, _: &mut alloy::providers::SendableTx<N>) {}

    async fn prepare<P: Provider<N>>(
        &self,
        provider: &P,
        tx: &<N as Network>::TransactionRequest,
    ) -> TransportResult<Self::Fillable> {
        match self.inner.prepare(provider, tx).await {
            Ok(fill) => Ok(Some(fill)),
            Err(_) => Ok(None),
        }
    }

    async fn fill(
        &self,
        fillable: Self::Fillable,
        mut tx: alloy::providers::SendableTx<N>,
    ) -> TransportResult<SendableTx<N>> {
        if let Some(fill) = fillable {
            let mut tx = self.inner.fill(fill, tx).await?;
            if let Some(builder) = tx.as_mut_builder() {
                if let Some(estimated) = builder.gas_limit() {
                    let padded = estimated
                        .checked_mul(GAS_ESTIMATE_PADDING_NUMERATOR)
                        .and_then(|v| v.checked_div(GAS_ESTIMATE_PADDING_DENOMINATOR))
                        .unwrap_or(u64::MAX);
                    builder.set_gas_limit(padded);
                }
            }
            Ok(tx)
        } else {
            if let Some(builder) = tx.as_mut_builder() {
                builder.set_gas_limit(self.default_gas_limit);
                builder.set_max_fee_per_gas(self.default_max_fee_per_gas);
                builder.set_max_priority_fee_per_gas(self.default_priority_fee);
            }
            Ok(tx)
        }
    }
}
