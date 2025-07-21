use std::collections::HashMap;

use alloy::{
    eips::BlockNumberOrTag,
    json_abi::JsonAbi,
    network::TransactionBuilder,
    primitives::{Address, Bytes, U256},
    rpc::types::TransactionRequest,
};
use semver::VersionReq;
use serde::Deserialize;
use serde_json::Value;

use revive_dt_node_interaction::EthereumNode;

use crate::metadata::ContractInstance;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Input {
    #[serde(default = "default_caller")]
    pub caller: Address,
    pub comment: Option<String>,
    #[serde(default = "default_instance")]
    pub instance: ContractInstance,
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

impl Calldata {
    pub fn find_all_contract_instances(&self, vec: &mut Vec<ContractInstance>) {
        if let Calldata::Compound(compound) = self {
            for item in compound {
                if let Some(instance) = item.strip_suffix(".address") {
                    vec.push(ContractInstance::new_from(instance))
                }
            }
        }
    }
}

impl ExpectedOutput {
    pub fn find_all_contract_instances(&self, vec: &mut Vec<ContractInstance>) {
        if let Some(ref cd) = self.return_data {
            cd.find_all_contract_instances(vec);
        }
    }
}

impl Input {
    fn instance_to_address(
        &self,
        instance: &ContractInstance,
        deployed_contracts: &HashMap<ContractInstance, (Address, JsonAbi)>,
    ) -> anyhow::Result<Address> {
        deployed_contracts
            .get(instance)
            .map(|(a, _)| *a)
            .ok_or_else(|| anyhow::anyhow!("instance {instance:?} not deployed"))
    }

    pub fn encoded_input(
        &self,
        deployed_contracts: &HashMap<ContractInstance, (Address, JsonAbi)>,
        chain_state_provider: &impl EthereumNode,
    ) -> anyhow::Result<Bytes> {
        match self.method {
            Method::Deployer | Method::Fallback => {
                let calldata_args = match &self.calldata {
                    Some(Calldata::Compound(args)) => args,
                    _ => anyhow::bail!("Expected compound calldata for function call"),
                };

                let mut calldata = Vec::<u8>::with_capacity(calldata_args.len() * 32);
                for (arg_idx, arg) in calldata_args.iter().enumerate() {
                    match resolve_argument(arg, deployed_contracts, chain_state_provider) {
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
            Method::FunctionName(ref function_name) => {
                let Some(abi) = deployed_contracts.get(&self.instance).map(|(_, a)| a) else {
                    tracing::error!(
                        contract_name = self.instance.as_ref(),
                        available_abis = ?deployed_contracts.keys().collect::<Vec<_>>(),
                        "Attempted to lookup ABI of contract but it wasn't found"
                    );
                    anyhow::bail!("ABI for instance '{}' not found", self.instance.as_ref());
                };

                tracing::trace!("ABI found for instance: {}", &self.instance.as_ref());

                // We follow the same logic that's implemented in the matter-labs-tester where they resolve
                // the function name into a function selector and they assume that he function doesn't have
                // any existing overloads.
                // https://github.com/matter-labs/era-compiler-tester/blob/1dfa7d07cba0734ca97e24704f12dd57f6990c2c/compiler_tester/src/test/case/input/mod.rs#L158-L190
                let function = abi
                    .functions()
                    .find(|function| function.signature().starts_with(function_name))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Function with name {:?} not found in ABI for the instance {:?}",
                            function_name,
                            &self.instance
                        )
                    })?;

                tracing::trace!("Functions found for instance: {}", self.instance.as_ref());

                let calldata_args = match &self.calldata {
                    Some(Calldata::Compound(args)) => args,
                    _ => anyhow::bail!("Expected compound calldata for function call"),
                };

                tracing::trace!(
                    "Starting encoding ABI's parameters for instance: {}",
                    self.instance.as_ref()
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
                    match resolve_argument(arg, deployed_contracts, chain_state_provider) {
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
        }
    }

    /// Parse this input into a legacy transaction.
    pub fn legacy_transaction(
        &self,
        deployed_contracts: &HashMap<ContractInstance, (Address, JsonAbi)>,
        chain_state_provider: &impl EthereumNode,
    ) -> anyhow::Result<TransactionRequest> {
        let input_data = self.encoded_input(deployed_contracts, chain_state_provider)?;
        let transaction_request = TransactionRequest::default();
        match self.method {
            Method::Deployer => Ok(transaction_request.with_deploy_code(input_data)),
            _ => Ok(transaction_request
                .to(self.instance_to_address(&self.instance, deployed_contracts)?)
                .input(input_data.into())),
        }
    }

    pub fn find_all_contract_instances(&self) -> Vec<ContractInstance> {
        let mut vec = Vec::new();
        vec.push(self.instance.clone());

        if let Some(ref cd) = self.calldata {
            cd.find_all_contract_instances(&mut vec);
        }
        match &self.expected {
            Some(Expected::Calldata(cd)) => {
                cd.find_all_contract_instances(&mut vec);
            }
            Some(Expected::Expected(expected)) => {
                expected.find_all_contract_instances(&mut vec);
            }
            Some(Expected::ExpectedMany(expected)) => {
                for expected in expected {
                    expected.find_all_contract_instances(&mut vec);
                }
            }
            None => {}
        }

        vec
    }
}

fn default_instance() -> ContractInstance {
    ContractInstance::new_from("Test")
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
    deployed_contracts: &HashMap<ContractInstance, (Address, JsonAbi)>,
    chain_state_provider: &impl EthereumNode,
) -> anyhow::Result<U256> {
    if let Some(instance) = value.strip_suffix(".address") {
        Ok(U256::from_be_slice(
            deployed_contracts
                .get(&ContractInstance::new_from(instance))
                .map(|(a, _)| *a)
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
        Ok(U256::from_str_radix(value, 16)
            .map_err(|error| anyhow::anyhow!("Invalid hexadecimal literal: {}", error))?)
    } else if value == "$CHAIN_ID" {
        let chain_id = chain_state_provider.chain_id()?;
        Ok(U256::from(chain_id))
    } else if value == "$GAS_LIMIT" {
        let gas_limit = chain_state_provider.block_gas_limit(BlockNumberOrTag::Latest)?;
        Ok(U256::from(gas_limit))
    } else if value == "$COINBASE" {
        let coinbase = chain_state_provider.block_coinbase(BlockNumberOrTag::Latest)?;
        Ok(U256::from_be_slice(coinbase.as_ref()))
    } else if value == "$DIFFICULTY" {
        let block_difficulty = chain_state_provider.block_difficulty(BlockNumberOrTag::Latest)?;
        Ok(block_difficulty)
    } else if value.starts_with("$BLOCK_HASH") {
        let offset: u64 = value
            .split(':')
            .next_back()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();

        let current_block_number = chain_state_provider.last_block_number()?;
        let desired_block_number = current_block_number - offset;

        let block_hash = chain_state_provider.block_hash(desired_block_number.into())?;

        Ok(U256::from_be_bytes(block_hash.0))
    } else if value == "$BLOCK_NUMBER" {
        let current_block_number = chain_state_provider.last_block_number()?;
        Ok(U256::from(current_block_number))
    } else if value == "$BLOCK_TIMESTAMP" {
        let timestamp = chain_state_provider.block_timestamp(BlockNumberOrTag::Latest)?;
        Ok(U256::from(timestamp))
    } else {
        Ok(U256::from_str_radix(value, 10)
            .map_err(|error| anyhow::anyhow!("Invalid decimal literal: {}", error))?)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use alloy::json_abi::JsonAbi;
    use alloy_primitives::address;
    use alloy_sol_types::SolValue;
    use std::collections::HashMap;

    struct DummyEthereumNode;

    impl EthereumNode for DummyEthereumNode {
        fn execute_transaction(
            &self,
            _: TransactionRequest,
        ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
            unimplemented!()
        }

        fn trace_transaction(
            &self,
            _: alloy::rpc::types::TransactionReceipt,
        ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
            unimplemented!()
        }

        fn state_diff(
            &self,
            _: alloy::rpc::types::TransactionReceipt,
        ) -> anyhow::Result<alloy::rpc::types::trace::geth::DiffMode> {
            unimplemented!()
        }

        fn fetch_add_nonce(&self, _: Address) -> anyhow::Result<u64> {
            unimplemented!()
        }

        fn chain_id(&self) -> anyhow::Result<alloy_primitives::ChainId> {
            Ok(0x123)
        }

        fn block_gas_limit(&self, _: alloy::eips::BlockNumberOrTag) -> anyhow::Result<u128> {
            Ok(0x1234)
        }

        fn block_coinbase(&self, _: alloy::eips::BlockNumberOrTag) -> anyhow::Result<Address> {
            Ok(Address::ZERO)
        }

        fn block_difficulty(&self, _: alloy::eips::BlockNumberOrTag) -> anyhow::Result<U256> {
            Ok(U256::from(0x12345u128))
        }

        fn block_hash(
            &self,
            _: alloy::eips::BlockNumberOrTag,
        ) -> anyhow::Result<alloy_primitives::BlockHash> {
            Ok([0xEE; 32].into())
        }

        fn block_timestamp(
            &self,
            _: alloy::eips::BlockNumberOrTag,
        ) -> anyhow::Result<alloy_primitives::BlockTimestamp> {
            Ok(0x123456)
        }

        fn last_block_number(&self) -> anyhow::Result<alloy_primitives::BlockNumber> {
            Ok(0x1234567)
        }
    }

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
            instance: ContractInstance::new_from("Contract"),
            method: Method::FunctionName("store".to_owned()),
            calldata: Some(Calldata::Compound(vec!["42".into()])),
            ..Default::default()
        };

        let mut contracts = HashMap::new();
        contracts.insert(
            ContractInstance::new_from("Contract"),
            (Address::ZERO, parsed_abi),
        );

        let encoded = input.encoded_input(&contracts, &DummyEthereumNode).unwrap();
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

        let input: Input = Input {
            instance: ContractInstance::new_from("Contract"),
            method: Method::FunctionName("send".to_owned()),
            calldata: Some(Calldata::Compound(vec![
                "0x1000000000000000000000000000000000000001".to_string(),
            ])),
            ..Default::default()
        };

        let mut contracts = HashMap::new();
        contracts.insert(
            ContractInstance::new_from("Contract"),
            (Address::ZERO, parsed_abi),
        );

        let encoded = input.encoded_input(&contracts, &DummyEthereumNode).unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (alloy_primitives::Address,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }

    #[test]
    fn resolver_can_resolve_chain_id_variable() {
        // Arrange
        let input = "$CHAIN_ID";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(DummyEthereumNode.chain_id().unwrap()))
    }

    #[test]
    fn resolver_can_resolve_gas_limit_variable() {
        // Arrange
        let input = "$GAS_LIMIT";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(
                DummyEthereumNode
                    .block_gas_limit(Default::default())
                    .unwrap()
            )
        )
    }

    #[test]
    fn resolver_can_resolve_coinbase_variable() {
        // Arrange
        let input = "$COINBASE";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from_be_slice(
                DummyEthereumNode
                    .block_coinbase(Default::default())
                    .unwrap()
                    .as_ref()
            )
        )
    }

    #[test]
    fn resolver_can_resolve_block_difficulty_variable() {
        // Arrange
        let input = "$DIFFICULTY";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            DummyEthereumNode
                .block_difficulty(Default::default())
                .unwrap()
        )
    }

    #[test]
    fn resolver_can_resolve_block_hash_variable() {
        // Arrange
        let input = "$BLOCK_HASH";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from_be_bytes(DummyEthereumNode.block_hash(Default::default()).unwrap().0)
        )
    }

    #[test]
    fn resolver_can_resolve_block_number_variable() {
        // Arrange
        let input = "$BLOCK_NUMBER";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(DummyEthereumNode.last_block_number().unwrap())
        )
    }

    #[test]
    fn resolver_can_resolve_block_timestamp_variable() {
        // Arrange
        let input = "$BLOCK_TIMESTAMP";

        // Act
        let resolved = resolve_argument(input, &Default::default(), &DummyEthereumNode);

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(
                DummyEthereumNode
                    .block_timestamp(Default::default())
                    .unwrap()
            )
        )
    }
}
