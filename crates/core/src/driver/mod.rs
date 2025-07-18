//! The test driver handles the compilation and execution of the test cases.

use std::collections::HashMap;
use std::marker::PhantomData;

use alloy::json_abi::JsonAbi;
use alloy::network::{Ethereum, TransactionBuilder};
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::GethTrace;
use alloy::{
    primitives::Address,
    rpc::types::{
        TransactionRequest,
        trace::geth::{AccountState, DiffMode},
    },
};
use anyhow::Context;
use indexmap::IndexMap;
use revive_dt_compiler::{Compiler, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::case::CaseIdx;
use revive_dt_format::input::Method;
use revive_dt_format::metadata::{ContractInstance, ContractPathAndIdentifier};
use revive_dt_format::{input::Input, metadata::Metadata, mode::SolcMode};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::reporter::{CompilationTask, Report, Span};
use revive_solc_json_interface::SolcStandardJsonOutput;
use serde_json::Value;
use tracing::Level;

use crate::Platform;
use crate::common::*;

pub struct State<'a, T: Platform> {
    /// The configuration that the framework was started with.
    ///
    /// This is currently used to get certain information from it such as the solc mode and other
    /// information used at runtime.
    config: &'a Arguments,

    /// The [`Span`] used in reporting.
    span: Span,

    /// A vector of all of the compiled contracts. Each call to [`build_contracts`] adds a new entry
    /// to this vector.
    ///
    /// [`build_contracts`]: State::build_contracts
    contracts: Vec<SolcStandardJsonOutput>,

    /// This map stores the contracts deployments that have been made for each case within a
    /// metadata file. Note, this means that the state can't be reused between different metadata
    /// files.
    deployed_contracts: HashMap<CaseIdx, HashMap<ContractInstance, (Address, JsonAbi)>>,

    phantom: PhantomData<T>,
}

impl<'a, T> State<'a, T>
where
    T: Platform,
{
    pub fn new(config: &'a Arguments, span: Span) -> Self {
        Self {
            config,
            span,
            contracts: Default::default(),
            deployed_contracts: Default::default(),
            phantom: Default::default(),
        }
    }

    /// Returns a copy of the current span.
    fn span(&self) -> Span {
        self.span
    }

    pub fn build_contracts(&mut self, mode: &SolcMode, metadata: &Metadata) -> anyhow::Result<()> {
        let mut span = self.span();
        span.next_metadata(
            metadata
                .file_path
                .as_ref()
                .expect("metadata should have been read from a file")
                .clone(),
        );

        let Some(version) = mode.last_patch_version(&self.config.solc) else {
            anyhow::bail!("unsupported solc version: {:?}", &mode.solc_version);
        };

        let compiler = Compiler::<T::Compiler>::new()
            .allow_path(metadata.directory()?)
            .solc_optimizer(mode.solc_optimize());

        let compiler = FilesWithExtensionIterator::new(metadata.directory()?)
            .with_allowed_extension("sol")
            .try_fold(compiler, |compiler, path| compiler.with_source(&path))?;

        let mut task = CompilationTask {
            json_input: compiler.input(),
            json_output: None,
            mode: mode.clone(),
            compiler_version: format!("{}", &version),
            error: None,
        };

        let compiler_path = T::Compiler::get_compiler_executable(self.config, version)?;
        match compiler.try_build(compiler_path) {
            Ok(output) => {
                task.json_output = Some(output.output.clone());
                task.error = output.error;
                self.contracts.push(output.output);

                if let Some(last_output) = self.contracts.last() {
                    if let Some(contracts) = &last_output.contracts {
                        for (file, contracts_map) in contracts {
                            for contract_name in contracts_map.keys() {
                                tracing::debug!(
                                    "Compiled contract: {contract_name} from file: {file}"
                                );
                            }
                        }
                    } else {
                        tracing::warn!("Compiled contracts field is None");
                    }
                }

                Report::compilation(span, T::config_id(), task);
                Ok(())
            }
            Err(error) => {
                tracing::error!("Failed to compile contract: {:?}", error.to_string());
                task.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub fn handle_input(
        &mut self,
        metadata: &Metadata,
        case_idx: CaseIdx,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        let deployment_receipts =
            self.handle_contract_deployment(metadata, case_idx, input, node)?;
        self.handle_input_execution(case_idx, input, deployment_receipts, node)
    }

    /// Handles the contract deployment for a given input performing it if it needs to be performed.
    fn handle_contract_deployment(
        &mut self,
        metadata: &Metadata,
        case_idx: CaseIdx,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<HashMap<ContractInstance, TransactionReceipt>> {
        let span = tracing::debug_span!(
            "Handling contract deployment",
            ?case_idx,
            instance = ?input.instance
        );
        let _guard = span.enter();

        // The ordering of the following statements and the use of the IndexMap is very intentional
        // here. The order in which we do the deployments matters. For example, say that this is
        // a `#deployer` call and the first argument is `Callable.address` which has not yet been
        // deployed. This means that we need to deploy it first and then deploy then deploy the one
        // from the input.
        let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
        for instance in input.find_all_contract_instances().into_iter() {
            if !self
                .deployed_contracts
                .entry(case_idx)
                .or_default()
                .contains_key(&instance)
            {
                instances_we_must_deploy.entry(instance).or_insert(false);
            }
        }
        if let Method::Deployer = input.method {
            instances_we_must_deploy.swap_remove(&input.instance);
            instances_we_must_deploy.insert(input.instance.clone(), true);
        }

        tracing::debug!(
            instances_to_deploy = instances_we_must_deploy.len(),
            "Computed the number of required deployments for input"
        );

        let mut receipts = HashMap::new();
        for (instance, deploy_with_constructor_arguments) in instances_we_must_deploy.into_iter() {
            // What we have at this moment is just a contract instance which is kind of like a variable
            // name for an actual underlying contract. So, we need to resolve this instance to the info
            // of the contract that it belongs to.
            let Some(ContractPathAndIdentifier {
                contract_source_path,
                contract_ident,
            }) = metadata.contract_sources()?.remove(&instance)
            else {
                tracing::error!("Contract source not found for instance");
                anyhow::bail!("Contract source not found for instance {:?}", instance)
            };

            let compiled_contract = self.contracts.iter().find_map(|output| {
                output
                    .contracts
                    .as_ref()?
                    .get(&contract_source_path.display().to_string())
                    .and_then(|source_file_contracts| {
                        source_file_contracts.get(contract_ident.as_ref())
                    })
            });
            let Some(code) = compiled_contract
                .and_then(|contract| contract.evm.as_ref().and_then(|evm| evm.bytecode.as_ref()))
            else {
                tracing::error!(
                    contract_source_path = contract_source_path.display().to_string(),
                    contract_ident = contract_ident.as_ref(),
                    "Failed to find bytecode for contract"
                );
                anyhow::bail!("Failed to find bytecode for contract {:?}", instance)
            };

            // TODO: When we want to do linking it would be best to do it at this stage here. We have
            // the context from the metadata files and therefore know what needs to be linked and in
            // what order it needs to happen.

            let mut code = match alloy::hex::decode(&code.object) {
                Ok(code) => code,
                Err(error) => {
                    tracing::error!(
                        ?error,
                        contract_source_path = contract_source_path.display().to_string(),
                        contract_ident = contract_ident.as_ref(),
                        "Failed to hex-decode byte code - This could possibly mean that the bytecode requires linking"
                    );
                    anyhow::bail!("Failed to hex-decode the byte code {}", error)
                }
            };

            if deploy_with_constructor_arguments {
                let encoded_input =
                    input.encoded_input(self.deployed_contracts.entry(case_idx).or_default())?;
                code.extend(encoded_input.to_vec());
            }

            let tx = {
                let tx = TransactionRequest::default().from(input.caller);
                TransactionBuilder::<Ethereum>::with_deploy_code(tx, code)
            };

            let receipt = match node.execute_transaction(tx) {
                Ok(receipt) => receipt,
                Err(error) => {
                    tracing::error!(
                        node = std::any::type_name::<T>(),
                        ?error,
                        "Contract deployment transaction failed."
                    );
                    return Err(error);
                }
            };

            let Some(address) = receipt.contract_address else {
                tracing::error!("Contract deployment transaction didn't return an address");
                anyhow::bail!("Contract deployment didn't return an address");
            };
            tracing::info!(
                instance_name = ?instance,
                instance_address = ?address,
                "Deployed contract"
            );

            let Some(Value::String(metadata)) =
                compiled_contract.and_then(|contract| contract.metadata.as_ref())
            else {
                tracing::error!("Contract does not have a metadata field");
                anyhow::bail!("Contract does not have a metadata field");
            };

            let Ok(metadata) = serde_json::from_str::<Value>(metadata) else {
                tracing::error!(%metadata, "Failed to parse solc metadata into a structured value");
                anyhow::bail!("Failed to parse solc metadata into a structured value {metadata}");
            };

            let Some(abi) = metadata.get("output").and_then(|value| value.get("abi")) else {
                tracing::error!(%metadata, "Failed to access the .output.abi field of the solc metadata");
                anyhow::bail!(
                    "Failed to access the .output.abi field of the solc metadata {metadata}"
                );
            };

            let Ok(abi) = serde_json::from_value::<JsonAbi>(abi.clone()) else {
                tracing::error!(%metadata, "Failed to deserialize ABI into a structured format");
                anyhow::bail!("Failed to deserialize ABI into a structured format {metadata}");
            };

            self.deployed_contracts
                .entry(case_idx)
                .or_default()
                .insert(instance.clone(), (address, abi));

            receipts.insert(instance.clone(), receipt);
        }

        Ok(receipts)
    }

    /// Handles the execution of the input in terms of the calls that need to be made.
    fn handle_input_execution(
        &mut self,
        case_idx: CaseIdx,
        input: &Input,
        deployment_receipts: HashMap<ContractInstance, TransactionReceipt>,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        tracing::trace!("Calling execute_input for input: {input:?}");

        let receipt = match input.method {
            // This input was already executed when `handle_input` was called. We just need to
            // lookup the transaction receipt in this case and continue on.
            Method::Deployer => deployment_receipts
                .get(&input.instance)
                .context("Failed to find deployment receipt")?
                .clone(),
            Method::Fallback | Method::FunctionName(_) => {
                let tx = match input
                    .legacy_transaction(self.deployed_contracts.entry(case_idx).or_default())
                {
                    Ok(tx) => {
                        tracing::debug!("Legacy transaction data: {tx:#?}");
                        tx
                    }
                    Err(err) => {
                        tracing::error!("Failed to construct legacy transaction: {err:?}");
                        return Err(err);
                    }
                };

                tracing::trace!("Executing transaction for input: {input:?}");

                match node.execute_transaction(tx) {
                    Ok(receipt) => receipt,
                    Err(err) => {
                        tracing::error!(
                            "Failed to execute transaction when executing the contract: {}, {:?}",
                            &*input.instance,
                            err
                        );
                        return Err(err);
                    }
                }
            }
        };

        tracing::trace!(
            "Transaction receipt for executed contract: {} - {:?}",
            &*input.instance,
            receipt,
        );

        let trace = node.trace_transaction(receipt.clone())?;
        tracing::trace!(
            "Trace result for contract: {} - {:?}",
            &*input.instance,
            trace
        );

        let diff = node.state_diff(receipt.clone())?;

        Ok((receipt, trace, diff))
    }
}

pub struct Driver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    config: &'a Arguments,
    leader_node: &'a Leader::Blockchain,
    follower_node: &'a Follower::Blockchain,
}

impl<'a, L, F> Driver<'a, L, F>
where
    L: Platform,
    F: Platform,
{
    pub fn new(
        metadata: &'a Metadata,
        config: &'a Arguments,
        leader_node: &'a L::Blockchain,
        follower_node: &'a F::Blockchain,
    ) -> Driver<'a, L, F> {
        Self {
            metadata,
            config,
            leader_node,
            follower_node,
        }
    }

    pub fn trace_diff_mode(label: &str, diff: &DiffMode) {
        tracing::trace!("{label} - PRE STATE:");
        for (addr, state) in &diff.pre {
            Self::trace_account_state("  [pre]", addr, state);
        }

        tracing::trace!("{label} - POST STATE:");
        for (addr, state) in &diff.post {
            Self::trace_account_state("  [post]", addr, state);
        }
    }

    fn trace_account_state(prefix: &str, addr: &Address, state: &AccountState) {
        tracing::trace!("{prefix} 0x{addr:x}");

        if let Some(balance) = &state.balance {
            tracing::trace!("{prefix}   balance: {balance}");
        }
        if let Some(nonce) = &state.nonce {
            tracing::trace!("{prefix}   nonce: {nonce}");
        }
        if let Some(code) = &state.code {
            tracing::trace!("{prefix}   code: {code}");
        }
    }

    pub fn execute(&mut self, span: Span) -> anyhow::Result<()> {
        for mode in self.metadata.solc_modes() {
            let mut leader_state = State::<L>::new(self.config, span);
            leader_state.build_contracts(&mode, self.metadata)?;

            let mut follower_state = State::<F>::new(self.config, span);
            follower_state.build_contracts(&mode, self.metadata)?;

            for (case_idx, case) in self.metadata.cases.iter().enumerate() {
                // Creating a tracing span to know which case within the metadata is being executed
                // and which one we're getting logs for.
                let tracing_span = tracing::span!(
                    Level::INFO,
                    "Executing case",
                    case = case.name,
                    case_idx = case_idx
                );
                let _guard = tracing_span.enter();

                let case_idx = CaseIdx::from(case_idx);
                for input in &case.inputs {
                    tracing::debug!("Starting executing contract {}", &*input.instance);
                    let (leader_receipt, _, leader_diff) = match leader_state.handle_input(
                        self.metadata,
                        case_idx,
                        input,
                        self.leader_node,
                    ) {
                        Ok(result) => result,
                        Err(err) => {
                            tracing::error!(
                                "Leader execution failed for {}: {err}",
                                *input.instance
                            );
                            continue;
                        }
                    };

                    let (follower_receipt, _, follower_diff) = match follower_state.handle_input(
                        self.metadata,
                        case_idx,
                        input,
                        self.follower_node,
                    ) {
                        Ok(result) => result,
                        Err(err) => {
                            tracing::error!(
                                "Follower execution failed for {}: {err}",
                                *input.instance
                            );
                            continue;
                        }
                    };

                    if leader_diff == follower_diff {
                        tracing::debug!("State diffs match between leader and follower.");
                    } else {
                        tracing::debug!("State diffs mismatch between leader and follower.");
                        Self::trace_diff_mode("Leader", &leader_diff);
                        Self::trace_diff_mode("Follower", &follower_diff);
                    }

                    if leader_receipt.logs() != follower_receipt.logs() {
                        tracing::debug!("Log/event mismatch between leader and follower.");
                        tracing::trace!("Leader logs: {:?}", leader_receipt.logs());
                        tracing::trace!("Follower logs: {:?}", follower_receipt.logs());
                    }

                    if leader_receipt.status() != follower_receipt.status() {
                        tracing::debug!(
                            "Mismatch in status: leader = {}, follower = {}",
                            leader_receipt.status(),
                            follower_receipt.status()
                        );
                    }
                }
            }
        }

        Ok(())
    }
}
