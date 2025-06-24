use std::collections::HashMap;

use alloy::{
    json_abi::Function,
    primitives::{Address, TxKind},
    rpc::types::TransactionRequest,
};
use semver::VersionReq;
use serde::{Deserialize, de::Deserializer};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Input {
    #[serde(default = "default_caller")]
    pub caller: Address,
    pub comment: Option<String>,
    #[serde(default = "default_instance")]
    pub instance: String,
    #[serde(deserialize_with = "deserialize_method")]
    pub method: Method,
    pub calldata: Option<Calldata>,
    pub expected: Option<Expected>,
    pub value: Option<String>,
    pub storage: Option<HashMap<String, Calldata>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Expected {
    Calldata(Calldata),
    Expected(ExpectedOutput),
    ExpectedMany(Vec<ExpectedOutput>),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ExpectedOutput {
    compiler_version: Option<VersionReq>,
    return_data: Option<Calldata>,
    events: Option<Value>,
    exception: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Calldata {
    Single(String),
    Compound(Vec<String>),
}

/// Specify how the contract is called.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub enum Method {
    /// Initiate a deploy transaction, calling contracts constructor.
    ///
    /// Indicated by `#deployer`.
    Deployer,
    /// Does not calculate and insert a function selector.
    ///
    /// Indicated by `#fallback`.
    #[default]
    Fallback,
    /// Call the public function with this selector.
    ///
    /// Calculates the selector if neither deployer or fallback matches.
    Function([u8; 4]),
}

fn deserialize_method<'de, D>(deserializer: D) -> Result<Method, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match String::deserialize(deserializer)?.as_str() {
        "#deployer" => Method::Deployer,
        "#fallback" => Method::Fallback,
        signature => {
            let signature = if signature.ends_with(')') {
                signature.to_string()
            } else {
                format!("{signature}()")
            };
            match Function::parse(&signature) {
                Ok(function) => Method::Function(function.selector().0),
                Err(error) => {
                    return Err(serde::de::Error::custom(format!(
                        "parsing function signature '{signature}' error: {error}"
                    )));
                }
            }
        }
    })
}

impl Input {
    fn instance_to_address(
        &self,
        instance: &str,
        deployed_contracts: &HashMap<String, Address>,
    ) -> anyhow::Result<Address> {
        deployed_contracts
            .get(instance)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("instance {instance} not deployed"))
    }

    /// Parse this input into a legacy transaction.
    pub fn legacy_transaction(
        &self,
        chain_id: u64,
        nonce: u64,
        deployed_contracts: &HashMap<String, Address>,
    ) -> anyhow::Result<TransactionRequest> {
        let to = match self.method {
            Method::Deployer => Some(TxKind::Create),
            _ => Some(TxKind::Call(
                self.instance_to_address(&self.instance, deployed_contracts)?,
            )),
        };

        Ok(TransactionRequest {
            from: Some(self.caller),
            to,
            nonce: Some(nonce),
            chain_id: Some(chain_id),
            gas_price: Some(5_000_000),
            gas: Some(5_000_000),
            ..Default::default()
        })
    }
}

fn default_instance() -> String {
    "Test".to_string()
}

fn default_caller() -> Address {
    "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap()
}
