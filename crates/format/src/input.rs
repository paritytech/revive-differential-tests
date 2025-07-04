use std::collections::HashMap;

use alloy::{
    hex,
    json_abi::{Function, JsonAbi},
    primitives::{Address, Bytes, TxKind},
    rpc::types::{TransactionInput, TransactionRequest},
};
use alloy_primitives::U256;
use alloy_sol_types::SolValue;
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
    Compound(Vec<CalldataArg>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum CalldataArg {
    Literal(String),
    AddressRef(String), // will be "Contract.address"
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

    pub fn encoded_input(
        &self,
        deployed_abis: &HashMap<String, JsonAbi>,
        deployed_contracts: &HashMap<String, Address>,
    ) -> anyhow::Result<Bytes> {
        let Method::Function(selector) = self.method else {
            return Ok(Bytes::default()); // fallback or deployer — no input
        };

        // ABI
        let abi = deployed_abis
            .get(&self.instance)
            .ok_or_else(|| anyhow::anyhow!("ABI for instance '{}' not found", &self.instance))?;

        // Find function by selector
        let function = abi
            .functions()
            .find(|f| f.selector().0 == selector)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Function with selector {:?} not found in ABI for the instance {:?}",
                    selector,
                    &self.instance
                )
            })?;

        // Parse calldata
        let calldata_args = match &self.calldata {
            Some(Calldata::Compound(args)) => args,
            _ => anyhow::bail!("Expected compound calldata for function call"),
        };

        if calldata_args.len() != function.inputs.len() {
            anyhow::bail!(
                "Function expects {} args, but got {}",
                function.inputs.len(),
                calldata_args.len()
            );
        }

        let mut encoded = selector.to_vec();

        for (i, param) in function.inputs.iter().enumerate() {
            let arg = calldata_args.get(i).unwrap();
            let encoded_arg = match arg {
                CalldataArg::Literal(value) => match param.ty.as_str() {
                    "uint256" | "uint" => {
                        let val: U256 = value.parse()?;
                        val.abi_encode()
                    }
                    "uint24" => {
                        let val: u32 = value.parse()?;
                        (val & 0xFFFFFF).abi_encode()
                    }
                    "bool" => {
                        let val: bool = value.parse()?;
                        val.abi_encode()
                    }
                    "address" => {
                        let addr: Address = value.parse()?;
                        addr.abi_encode()
                    }
                    "string" => value.abi_encode(),
                    "bytes32" => {
                        let val = hex::decode(value.trim_start_matches("0x"))?;
                        let mut fixed = [0u8; 32];
                        fixed[..val.len()].copy_from_slice(&val);
                        fixed.abi_encode()
                    }
                    "uint256[]" | "uint[]" => {
                        let nums: Vec<u64> = serde_json::from_str(value)?;
                        nums.abi_encode()
                    }
                    "bytes" => {
                        let val = hex::decode(value.trim_start_matches("0x"))?;
                        val.abi_encode()
                    }
                    _ => anyhow::bail!("Unsupported type: {}", param.ty),
                },
                CalldataArg::AddressRef(name) => {
                    let contract_name = name.trim_end_matches(".address");
                    let addr = deployed_contracts
                        .get(contract_name)
                        .copied()
                        .ok_or_else(|| {
                            anyhow::anyhow!("Address for '{}' not found", contract_name)
                        })?;
                    addr.abi_encode()
                }
            };

            encoded.extend(encoded_arg);
        }

        Ok(Bytes::from(encoded))
    }

    /// Parse this input into a legacy transaction.
    pub fn legacy_transaction(
        &self,
        chain_id: u64,
        nonce: u64,
        deployed_contracts: &HashMap<String, Address>,
        deployed_abis: &HashMap<String, JsonAbi>,
    ) -> anyhow::Result<TransactionRequest> {
        let to = match self.method {
            Method::Deployer => Some(TxKind::Create),
            _ => Some(TxKind::Call(
                self.instance_to_address(&self.instance, deployed_contracts)?,
            )),
        };

        let input_data = self.encoded_input(deployed_abis, deployed_contracts)?;

        Ok(TransactionRequest {
            from: Some(self.caller),
            to,
            nonce: Some(nonce),
            chain_id: Some(chain_id),
            gas_price: Some(5_000_000),
            gas: Some(5_000_000),
            input: TransactionInput::new(input_data),
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
