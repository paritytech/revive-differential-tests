use std::{collections::HashMap, str::FromStr};

use alloy::{
    json_abi::JsonAbi,
    primitives::{Address, Bytes, U256},
    rpc::types::{TransactionInput, TransactionRequest},
};
use alloy_primitives::TxKind;
use semver::VersionReq;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Input {
    #[serde(default = "default_caller")]
    pub caller: Address,
    pub comment: Option<String>,
    #[serde(default = "default_instance")]
    pub instance: String,
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
#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub enum Method {
    /// Initiate a deploy transaction, calling contracts constructor.
    ///
    /// Indicated by `#deployer`.
    #[serde(rename = "#deployer")]
    Deployer,

    /// Does not calculate and insert a function selector.
    ///
    /// Indicated by `#fallback`.
    #[default]
    #[serde(rename = "#fallback")]
    Fallback,

    /// Call the public function with the given name.
    #[serde(untagged)]
    FunctionName(String),
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
        let Method::FunctionName(ref function_name) = self.method else {
            return Ok(Bytes::default()); // fallback or deployer — no input
        };

        let abi = deployed_abis
            .get(&self.instance)
            .ok_or_else(|| anyhow::anyhow!("ABI for instance '{}' not found", &self.instance))?;

        tracing::trace!("ABI found for instance: {}", &self.instance);

        // We follow the same logic that's implemented in the matter-labs-tester where they resolve
        // the function name into a function selector and they assume that he function doesn't have
        // any existing overloads.
        // https://github.com/matter-labs/era-compiler-tester/blob/1dfa7d07cba0734ca97e24704f12dd57f6990c2c/compiler_tester/src/test/case/input/mod.rs#L158-L190
        let function = abi
            .functions()
            .find(|function| function.name.starts_with(function_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Function with name {:?} not found in ABI for the instance {:?}",
                    function_name,
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

        // Allocating a vector that we will be using for the calldata. The vector size will be:
        // 4 bytes for the function selector.
        // function.inputs.len() * 32 bytes for the arguments (each argument is a U256).
        //
        // We're using indices in the following code in order to avoid the need for us to allocate
        // a new buffer for each one of the resolved arguments.
        let mut calldata = Vec::<u8>::with_capacity(4 + calldata_args.len() * 32);
        calldata.extend(function.selector().0);

        for (arg_idx, arg) in calldata_args.iter().enumerate() {
            match resolve_argument(arg, deployed_contracts) {
                Ok(resolved) => {
                    calldata.extend(resolved.to_be_bytes::<32>());
                }
                Err(error) => {
                    tracing::error!(arg, arg_idx, ?error, "Failed to resolve argument");
                    return Err(error);
                }
            };
        }

        Ok(calldata.into())
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

/// This function takes in the string calldata argument provided in the JSON input and resolves it
/// into a [`U256`] which is later used to construct the calldata.
///
/// # Note
///
/// This piece of code is taken from the matter-labs-tester repository which is licensed under MIT
/// or Apache. The original source code can be found here:
/// https://github.com/matter-labs/era-compiler-tester/blob/0ed598a27f6eceee7008deab3ff2311075a2ec69/compiler_tester/src/test/case/input/value.rs#L43-L146
fn resolve_argument(
    value: &str,
    deployed_contracts: &HashMap<String, Address>,
) -> anyhow::Result<U256> {
    if let Some(instance) = value.strip_suffix(".address") {
        Ok(U256::from_be_slice(
            deployed_contracts
                .get(instance)
                .ok_or_else(|| anyhow::anyhow!("Instance `{}` not found", instance))?
                .as_ref(),
        ))
    } else if let Some(value) = value.strip_prefix('-') {
        let value = U256::from_str_radix(value, 10)
            .map_err(|error| anyhow::anyhow!("Invalid decimal literal after `-`: {}", error))?;
        if value > U256::ONE << 255u8 {
            anyhow::bail!("Decimal literal after `-` is too big");
        }
        let value = value
            .checked_sub(U256::ONE)
            .ok_or_else(|| anyhow::anyhow!("`-0` is invalid literal"))?;
        Ok(U256::MAX.checked_sub(value).expect("Always valid"))
    } else if let Some(value) = value.strip_prefix("0x") {
        Ok(U256::from_str(value)
            .map_err(|error| anyhow::anyhow!("Invalid hexadecimal literal: {}", error))?)
    } else {
        // TODO: This is a set of "variables" that we need to be able to resolve to be fully in
        // compliance with the matter labs tester but we currently do not resolve them. We need to
        // add logic that does their resolution in the future, perhaps through some kind of system
        // context API that we pass down to the resolution function that allows it to make calls to
        // the node to perform these resolutions.
        let is_unsupported = [
            "$CHAIN_ID",
            "$GAS_LIMIT",
            "$COINBASE",
            "$DIFFICULTY",
            "$BLOCK_HASH",
            "$BLOCK_TIMESTAMP",
        ]
        .iter()
        .any(|var| value.starts_with(var));

        if is_unsupported {
            tracing::error!(value, "Unsupported variable used");
            anyhow::bail!("Encountered {value} which is currently unsupported by the framework");
        } else {
            Ok(U256::from_str_radix(value, 10)
                .map_err(|error| anyhow::anyhow!("Invalid decimal literal: {}", error))?)
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use alloy::json_abi::JsonAbi;
    use alloy_primitives::address;
    use alloy_sol_types::SolValue;
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
        let selector = parsed_abi
            .function("store")
            .unwrap()
            .first()
            .unwrap()
            .selector()
            .0;

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::FunctionName("store".to_owned()),
            calldata: Some(Calldata::Compound(vec!["42".into()])),
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
        let selector = parsed_abi
            .function("send")
            .unwrap()
            .first()
            .unwrap()
            .selector()
            .0;

        let input = Input {
            instance: "Contract".to_string(),
            method: Method::FunctionName("send".to_owned()),
            calldata: Some(Calldata::Compound(vec![
                "0x1000000000000000000000000000000000000001".to_string(),
            ])),
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
