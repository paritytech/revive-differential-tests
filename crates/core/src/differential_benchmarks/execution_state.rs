use std::{collections::HashMap, path::PathBuf};

use alloy::{
	json_abi::JsonAbi,
	primitives::{Address, U256},
};

use revive_dt_format::metadata::{ContractIdent, ContractInstance};

#[derive(Clone)]
/// The state associated with the test execution of one of the workloads.
pub struct ExecutionState {
	/// The compiled contracts, these contracts have been compiled and have had the libraries
	/// linked against them and therefore they're ready to be deployed on-demand.
	pub compiled_contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,

	/// A map of all of the deployed contracts and information about them.
	pub deployed_contracts: HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,

	/// This map stores the variables used for each one of the cases contained in the metadata
	/// file.
	pub variables: HashMap<String, U256>,
}

impl ExecutionState {
	pub fn new(
		compiled_contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,
		deployed_contracts: HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,
	) -> Self {
		Self { compiled_contracts, deployed_contracts, variables: Default::default() }
	}

	pub fn empty() -> Self {
		Self {
			compiled_contracts: Default::default(),
			deployed_contracts: Default::default(),
			variables: Default::default(),
		}
	}
}
