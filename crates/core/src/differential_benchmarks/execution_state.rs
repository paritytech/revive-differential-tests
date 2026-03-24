use crate::internal_prelude::*;

/// The compiled contract artifacts keyed by source file path and contract name.
type CompiledContracts = HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>;

#[derive(Clone)]
/// The state associated with the test execution of one of the workloads.
pub struct ExecutionState {
    /// The compiled contracts, these contracts have been compiled and have had the libraries linked
    /// against them and therefore they're ready to be deployed on-demand.
    ///
    /// Wrapped in an [`Arc`] and [`tokio::sync::Mutex`] to allow sharing across concurrent drivers
    /// without cloning the potentially large compilation artifacts.
    pub compiled_contracts: Arc<tokio::sync::Mutex<CompiledContracts>>,

    /// A map of all of the deployed contracts and information about them.
    ///
    /// The value is wrapped in an [`Arc`] to allow cheap cloning of the map across concurrent
    /// drivers without deep-cloning the potentially large [`JsonAbi`] data.
    pub deployed_contracts: HashMap<ContractInstance, Arc<(ContractIdent, Address, JsonAbi)>>,

    /// This map stores the variables used for each one of the cases contained in the metadata file.
    pub variables: HashMap<String, U256>,
}

impl ExecutionState {
    pub fn new(
        compiled_contracts: CompiledContracts,
        deployed_contracts: HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,
    ) -> Self {
        Self {
            compiled_contracts: Arc::new(tokio::sync::Mutex::new(compiled_contracts)),
            deployed_contracts: deployed_contracts
                .into_iter()
                .map(|(k, v)| (k, Arc::new(v)))
                .collect(),
            variables: Default::default(),
        }
    }

    pub fn empty() -> Self {
        Self {
            compiled_contracts: Default::default(),
            deployed_contracts: Default::default(),
            variables: Default::default(),
        }
    }

    pub fn add_state(
        &mut self,
        Self {
            compiled_contracts: _,
            deployed_contracts,
            variables,
        }: Self,
    ) {
        self.deployed_contracts.extend(deployed_contracts);
        self.variables.extend(variables);
    }
}
