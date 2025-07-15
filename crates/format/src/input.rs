use std::collections::HashMap;

use alloy::{
    hex,
    json_abi::{Function, JsonAbi},
    network::TransactionBuilder,
    primitives::{Address, Bytes},
    rpc::types::TransactionRequest,
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
    /// For example: `Contract.address`
    AddressRef(String),
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

        let Some(abi) = deployed_abis.get(&self.instance) else {
            tracing::error!(
                contract_name = self.instance,
                available_abis = ?deployed_abis.keys().collect::<Vec<_>>(),
                "Attempted to lookup ABI of contract but it wasn't found"
            );
            anyhow::bail!("ABI for instance '{}' not found", &self.instance);
        };

        tracing::trace!("ABI found for instance: {}", &self.instance);

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

        tracing::trace!("Functions found for instance: {}", &self.instance);

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

        tracing::trace!(
            "Starting encoding ABI's parameters for instance: {}",
            &self.instance
        );

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
        nonce: u64,
        deployed_contracts: &HashMap<String, Address>,
        deployed_abis: &HashMap<String, JsonAbi>,
    ) -> anyhow::Result<TransactionRequest> {
        let input_data = self.encoded_input(deployed_abis, deployed_contracts)?;
        let transaction_request = TransactionRequest::default().nonce(nonce);
        match self.method {
            Method::Deployer => Ok(transaction_request.with_deploy_code(input_data)),
            _ => Ok(transaction_request
                .to(self.instance_to_address(&self.instance, deployed_contracts)?)
                .input(input_data.into())),
        }
    }
}

fn default_instance() -> String {
    "Test".to_string()
}

fn default_caller() -> Address {
    "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1".parse().unwrap()
}

#[cfg(test)]
mod tests {

    use super::*;
    use alloy::json_abi::JsonAbi;
    use alloy_primitives::{address, keccak256};
    use std::collections::HashMap;

    #[test]
    fn test_encoded_input_uint256() {
        let raw_metadata = r#"
            [
                {
                    "inputs": [{"name": "value", "type": "uint256"}],
                    "name": "store",
                    "outputs": [],
                    "stateMutability": "nonpayable",
                    "type": "function"
                }
            ]
        "#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_metadata).unwrap();
        let selector = keccak256("store(uint256)".as_bytes())[0..4]
            .try_into()
            .unwrap();

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::Function(selector),
            calldata: Some(Calldata::Compound(vec![CalldataArg::Literal(
                "42".to_string(),
            )])),
            ..Default::default()
        };

        let mut deployed_abis = HashMap::new();
        deployed_abis.insert("Contract".to_string(), parsed_abi);
        let deployed_contracts = HashMap::new();

        let encoded = input
            .encoded_input(&deployed_abis, &deployed_contracts)
            .unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (u64,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(decoded.0, 42);
    }

    #[test]
    fn test_encoded_input_bool() {
        let raw_abi = r#"[
            {
                "inputs": [{"name": "flag", "type": "bool"}],
                "name": "toggle",
                "outputs": [],
                "stateMutability": "nonpayable",
                "type": "function"
            }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = keccak256("toggle(bool)".as_bytes())[0..4]
            .try_into()
            .unwrap();

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::Function(selector),
            calldata: Some(Calldata::Compound(vec![CalldataArg::Literal(
                "true".to_string(),
            )])),
            ..Default::default()
        };

        let mut abis = HashMap::new();
        abis.insert("Contract".to_string(), parsed_abi);
        let contracts = HashMap::new();

        let encoded = input.encoded_input(&abis, &contracts).unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (bool,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(decoded.0, true);
    }

    #[test]
    fn test_encoded_input_string() {
        let raw_abi = r#"[
            {
                "inputs": [{"name": "msg", "type": "string"}],
                "name": "echo",
                "outputs": [],
                "stateMutability": "nonpayable",
                "type": "function"
            }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = keccak256("echo(string)".as_bytes())[0..4]
            .try_into()
            .unwrap();

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::Function(selector),
            calldata: Some(Calldata::Compound(vec![CalldataArg::Literal(
                "hello".to_string(),
            )])),
            ..Default::default()
        };

        let mut abis = HashMap::new();
        abis.insert("Contract".to_string(), parsed_abi);
        let contracts = HashMap::new();

        let encoded = input.encoded_input(&abis, &contracts).unwrap();
        assert!(encoded.0.starts_with(&selector));
    }

    #[test]
    fn test_encoded_input_uint256_array() {
        let raw_abi = r#"[
        {
            "inputs": [{"name": "arr", "type": "uint256[]"}],
            "name": "sum",
            "outputs": [],
            "stateMutability": "nonpayable",
            "type": "function"
        }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = keccak256("sum(uint256[])".as_bytes())[0..4]
            .try_into()
            .unwrap();

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::Function(selector),
            calldata: Some(Calldata::Compound(vec![CalldataArg::Literal(
                "[1,2,3]".to_string(),
            )])),
            ..Default::default()
        };

        let mut abis = HashMap::new();
        abis.insert("Contract".to_string(), parsed_abi);
        let contracts = HashMap::new();

        let encoded = input.encoded_input(&abis, &contracts).unwrap();
        assert!(encoded.0.starts_with(&selector));
    }

    #[test]
    fn test_encoded_input_address() {
        let raw_abi = r#"[
        {
            "inputs": [{"name": "recipient", "type": "address"}],
            "name": "send",
            "outputs": [],
            "stateMutability": "nonpayable",
            "type": "function"
        }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = keccak256("send(address)".as_bytes())[0..4]
            .try_into()
            .unwrap();

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::Function(selector),
            calldata: Some(Calldata::Compound(vec![CalldataArg::Literal(
                "0x1000000000000000000000000000000000000001".to_string(),
            )])),
            ..Default::default()
        };

        let mut abis = HashMap::new();
        abis.insert("Contract".to_string(), parsed_abi);
        let contracts = HashMap::new();

        let encoded = input.encoded_input(&abis, &contracts).unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (alloy_primitives::Address,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }
}
