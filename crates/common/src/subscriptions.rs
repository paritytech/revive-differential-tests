use crate::internal_prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MinedBlockInformation {
    pub ethereum_block_information: EthereumMinedBlockInformation,
    pub substrate_block_information: Option<SubstrateMinedBlockInformation>,
    pub tx_counts: BTreeMap<StepPath, usize>,
    pub observation_time: SystemTime,
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
    pub block_number: BlockNumber,
    pub block_timestamp: BlockTimestamp,
    pub mined_gas: u128,
    pub block_gas_limit: u128,
    pub transaction_hashes: Vec<TxHash>,
}

impl EthereumMinedBlockInformation {
    pub fn gas_block_fullness_percentage(&self) -> u8 {
        (self.mined_gas * 100 / self.block_gas_limit) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubstrateMinedBlockInformation {
    pub ref_time: u128,
    pub max_ref_time: u64,
    pub proof_size: u128,
    pub max_proof_size: u64,
    pub block_hash: [u8; 32],
    #[serde(default)]
    pub pre_dispatch_ref_time: u128,
    #[serde(default)]
    pub pre_dispatch_proof_size: u128,
    pub is_last_block_in_slot: bool,
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
    #[rustfmt::skip]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, FromStr, DeriveMoreDisplay)]
    pub struct StepIdx(usize);
);

define_wrapper_type!(
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    pub struct StepPath(Vec<StepIdx>);
);

impl StepPath {
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
            .collect::<anyhow::Result<Vec<_>, _>>()
            .context("Failed to parse step path")
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
