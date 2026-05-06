use crate::internal_prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MinedBlockInformation {
    pub ethereum_block_information: EthereumMinedBlockInformation,
    pub substrate_block_information: Option<SubstrateMinedBlockInformation>,
    pub tx_counts: BTreeMap<StepPath, usize>,
    pub observation_time: SystemTime,

    /// The number of transactions pending in the transaction pool at the time this block was
    /// observed. Populated by the benchmark watcher; defaults to zero when not measured.
    #[serde(default)]
    pub pending_transaction_count: usize,
}

impl MinedBlockInformation {
    pub fn gas_block_fullness_percentage(&self) -> u8 {
        self.ethereum_block_information
            .gas_block_fullness_percentage()
    }

    pub fn ref_time_block_fullness_percentage(&self) -> Option<u8> {
        self.substrate_block_information
            .as_ref()
            .map(|block| block.ref_time_block_fullness_percentage())
    }

    pub fn proof_size_block_fullness_percentage(&self) -> Option<u8> {
        self.substrate_block_information
            .as_ref()
            .map(|block| block.proof_size_block_fullness_percentage())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EthereumMinedBlockInformation {
    /// The block number.
    pub block_number: BlockNumber,

    /// The block timestamp.
    pub block_timestamp: BlockTimestamp,

    /// The amount of gas mined in the block.
    pub mined_gas: u128,

    /// The gas limit of the block.
    pub block_gas_limit: u128,

    /// The hashes of the transactions that were mined as part of the block.
    pub transaction_hashes: Vec<TxHash>,
}

impl EthereumMinedBlockInformation {
    pub fn gas_block_fullness_percentage(&self) -> u8 {
        (self.mined_gas * 100 / self.block_gas_limit) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubstrateMinedBlockInformation {
    /// The ref time for substrate based chains.
    pub ref_time: u128,

    /// The max ref time for substrate based chains.
    pub max_ref_time: u64,

    /// The proof size for substrate based chains.
    pub proof_size: u128,

    /// The max proof size for substrate based chains.
    pub max_proof_size: u64,

    /// The block hash of the substrate block.
    pub block_hash: [u8; 32],

    /// The total pre-dispatch ref time weight of all transactions in the block, obtained from the
    /// execution tracer's `baseCallWeight` field. Defaults to zero when the tracer is unavailable.
    #[serde(default)]
    pub pre_dispatch_ref_time: u128,

    /// The total pre-dispatch proof size weight of all transactions in the block, obtained from the
    /// execution tracer's `baseCallWeight` field. Defaults to zero when the tracer is unavailable.
    #[serde(default)]
    pub pre_dispatch_proof_size: u128,
}

impl SubstrateMinedBlockInformation {
    pub fn ref_time_block_fullness_percentage(&self) -> u8 {
        (self.ref_time * 100 / self.max_ref_time as u128) as u8
    }

    pub fn proof_size_block_fullness_percentage(&self) -> u8 {
        (self.proof_size * 100 / self.max_proof_size as u128) as u8
    }
}

define_wrapper_type!(
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    pub struct StepIdx(usize) impl Display, FromStr;
);

define_wrapper_type!(
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    pub struct StepPath(Vec<StepIdx>);
);

impl StepPath {
    pub fn from_iterator(path: impl IntoIterator<Item = impl Into<StepIdx>>) -> Self {
        Self(path.into_iter().map(|value| value.into()).collect())
    }

    pub fn increment(&self) -> Self {
        let mut this = self.clone();
        if let Some(last) = this.last_mut() {
            last.0 += 1
        }
        this
    }

    pub fn append(&self, step_idx: impl Into<StepIdx>) -> Self {
        let mut this = self.clone();
        this.0.push(step_idx.into());
        this
    }
}

impl Display for StepPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .iter()
            .map(|idx| idx.to_string())
            .collect::<Vec<_>>()
            .join(".")
            .fmt(f)
    }
}

impl FromStr for StepPath {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split(".")
            .map(StepIdx::from_str)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Self)
    }
}

impl From<StepPath> for String {
    fn from(value: StepPath) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for StepPath {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
