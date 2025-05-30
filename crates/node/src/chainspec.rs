use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ChainSpec {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "chainType")]
    pub chain_type: String,
    #[serde(rename = "bootNodes")]
    pub boot_nodes: Vec<String>,
    #[serde(rename = "telemetryEndpoints")]
    pub telemetry_endpoints: Vec<(String, u64)>,
    #[serde(rename = "protocolId")]
    pub protocol_id: Option<String>,
    #[serde(rename = "properties")]
    pub properties: Option<serde_json::Value>,
    #[serde(rename = "forkBlocks")]
    pub fork_blocks: Option<serde_json::Value>,
    #[serde(rename = "badBlocks")]
    pub bad_blocks: Option<serde_json::Value>,
    #[serde(rename = "lightSyncState")]
    pub light_sync_state: Option<serde_json::Value>,
    #[serde(rename = "codeSubstitutes")]
    pub code_substitutes: serde_json::Value,
    #[serde(rename = "genesis")]
    pub genesis: Genesis,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Genesis {
    #[serde(rename = "runtimeGenesis")]
    pub runtime_genesis: RuntimeGenesis,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RuntimeGenesis {
    #[serde(rename = "code")]
    pub code: String,
    #[serde(rename = "patch")]
    pub patch: Patch,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Patch {
    #[serde(rename = "assets")]
    pub assets: Assets,
    #[serde(rename = "babe")]
    pub babe: Babe,
    #[serde(rename = "balances")]
    pub balances: Balances,
    #[serde(rename = "elections")]
    pub elections: Elections,
    #[serde(rename = "nominationPools")]
    pub nomination_pools: NominationPools,
    #[serde(rename = "revive")]
    pub revive: Revive,
    #[serde(rename = "session")]
    pub session: Session,
    #[serde(rename = "society")]
    pub society: Society,
    #[serde(rename = "staking")]
    pub staking: Staking,
    #[serde(rename = "sudo")]
    pub sudo: Sudo,
    #[serde(rename = "technicalCommittee")]
    pub technical_committee: TechnicalCommittee,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Assets {
    #[serde(rename = "accounts")]
    pub accounts: Vec<serde_json::Value>,
    #[serde(rename = "assets")]
    pub assets: Vec<(u64, String, bool, u64)>,
    #[serde(rename = "metadata")]
    pub metadata: Vec<serde_json::Value>,
    #[serde(rename = "nextAssetId")]
    pub next_asset_id: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Babe {
    #[serde(rename = "epochConfig")]
    pub epoch_config: EpochConfig,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EpochConfig {
    #[serde(rename = "allowed_slots")]
    pub allowed_slots: String,
    #[serde(rename = "c")]
    pub c: (u64, u64),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Balances {
    #[serde(rename = "balances")]
    pub balances: Vec<(String, u128)>,
    #[serde(rename = "devAccounts")]
    pub dev_accounts: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Elections {
    #[serde(rename = "members")]
    pub members: Vec<(String, u128)>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NominationPools {
    #[serde(rename = "minCreateBond")]
    pub min_create_bond: u128,
    #[serde(rename = "minJoinBond")]
    pub min_join_bond: u128,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Revive {
    #[serde(rename = "mappedAccounts")]
    pub mapped_accounts: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Session {
    #[serde(rename = "keys")]
    pub keys: Vec<(String, String, AuthorityKeys)>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AuthorityKeys {
    #[serde(rename = "authority_discovery")]
    pub authority_discovery: String,
    #[serde(rename = "babe")]
    pub babe: String,
    #[serde(rename = "beefy")]
    pub beefy: String,
    #[serde(rename = "grandpa")]
    pub grandpa: String,
    #[serde(rename = "im_online")]
    pub im_online: String,
    #[serde(rename = "mixnet")]
    pub mixnet: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Society {
    #[serde(rename = "pot")]
    pub pot: u128,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Staking {
    #[serde(rename = "invulnerables")]
    pub invulnerables: Vec<String>,
    #[serde(rename = "minimumValidatorCount")]
    pub minimum_validator_count: u32,
    #[serde(rename = "slashRewardFraction")]
    pub slash_reward_fraction: u128,
    #[serde(rename = "stakers")]
    pub stakers: Vec<(String, String, u128, String)>,
    #[serde(rename = "validatorCount")]
    pub validator_count: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Sudo {
    #[serde(rename = "key")]
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TechnicalCommittee {
    #[serde(rename = "members")]
    pub members: Vec<String>,
}
