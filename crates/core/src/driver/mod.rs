//! The test driver handles the compilation and execution of the test cases.

use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::path::PathBuf;

use alloy::json_abi::JsonAbi;
use alloy::network::{Ethereum, TransactionBuilder};
use alloy::primitives::U256;
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::{
    CallFrame, GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingOptions, GethTrace,
    PreStateConfig,
};
use alloy::{
    primitives::Address,
    rpc::types::{
        TransactionRequest,
        trace::geth::{AccountState, DiffMode},
    },
};
use anyhow::Context;
use indexmap::IndexMap;
use semver::Version;

use revive_dt_common::iterators::FilesWithExtensionIterator;
use revive_dt_compiler::{Compiler, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::case::{Case, CaseIdx};
use revive_dt_format::input::{Calldata, EtherValue, Expected, ExpectedOutput, Method};
use revive_dt_format::metadata::{ContractInstance, ContractPathAndIdent};
use revive_dt_format::{input::Input, metadata::Metadata, mode::SolcMode};
use revive_dt_node::Node;
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::reporter::{CompilationTask, Report, Span};

use crate::Platform;

pub struct CaseState<'a, T: Platform> {
    /// The configuration that the framework was started with.
    ///
    /// This is currently used to get certain information from it such as the solc mode and other
    /// information used at runtime.
    config: &'a Arguments,

    /// The [`Span`] used in reporting.
    span: Span,

    /// A map of all of the compiled contracts for the given metadata file.
    compiled_contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,

    /// This map stores the contracts deployments that have been made for each case within a
    /// metadata file. Note, this means that the state can't be reused between different metadata
    /// files.
    deployed_contracts: HashMap<CaseIdx, HashMap<ContractInstance, (Address, JsonAbi)>>,

    /// This map stores the variables used for each one of the cases contained in the metadata
    /// file.
    variables: HashMap<CaseIdx, HashMap<String, U256>>,

    /// This is a map of the deployed libraries.
    ///
    /// This map is not per case, but rather, per metadata file. This means that we do not redeploy
    /// the libraries with each case.
    deployed_libraries: HashMap<ContractInstance, (Address, JsonAbi)>,

    /// Stores the version of the compiler used for the given Solc mode.
    compiler_version: HashMap<&'a SolcMode, Version>,

    phantom: PhantomData<T>,
}

impl<'a, T> CaseState<'a, T>
where
    T: Platform,
{
    pub fn new(config: &'a Arguments, span: Span) -> Self {
        Self {
            config,
            span,
            compiled_contracts: Default::default(),
            deployed_contracts: Default::default(),
            variables: Default::default(),
            deployed_libraries: Default::default(),
            compiler_version: Default::default(),
            phantom: Default::default(),
        }
    }

    /// Returns a copy of the current span.
    fn span(&self) -> Span {
        self.span
    }

    pub fn build_contracts(
        &mut self,
        mode: &'a SolcMode,
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        let mut span = self.span();
        span.next_metadata(
            metadata
                .file_path
                .as_ref()
                .expect("metadata should have been read from a file")
                .clone(),
        );

        let compiler_version_or_requirement =
            mode.compiler_version_to_use(self.config.solc.clone());
        let compiler_path =
            T::Compiler::get_compiler_executable(self.config, compiler_version_or_requirement)?;
        let compiler_version = T::Compiler::new(compiler_path.clone()).version()?;
        self.compiler_version.insert(mode, compiler_version.clone());

        tracing::info!(%compiler_version, "Resolved the compiler version to use");

        let compiler = Compiler::<T::Compiler>::new()
            .with_allow_path(metadata.directory()?)
            .with_optimization(mode.solc_optimize());
        let mut compiler = metadata
            .files_to_compile()?
            .try_fold(compiler, |compiler, path| compiler.with_source(&path))?;
        for (library_instance, (library_address, _)) in self.deployed_libraries.iter() {
            let library_ident = &metadata
                .contracts
                .as_ref()
                .and_then(|contracts| contracts.get(library_instance))
                .expect("Impossible for library to not be found in contracts")
                .contract_ident;

            // Note the following: we need to tell solc which files require the libraries to be
            // linked into them. We do not have access to this information and therefore we choose
            // an easier, yet more compute intensive route, of telling solc that all of the files
            // need to link the library and it will only perform the linking for the files that do
            // actually need the library.
            compiler = FilesWithExtensionIterator::new(metadata.directory()?)
                .with_allowed_extension("sol")
                .fold(compiler, |compiler, path| {
                    compiler.with_library(&path, library_ident.as_str(), *library_address)
                });
        }

        let mut task = CompilationTask {
            json_input: compiler.input(),
            json_output: None,
            mode: mode.clone(),
            compiler_version: format!("{}", &compiler_version),
            error: None,
        };

        match compiler.try_build(compiler_path) {
            Ok(output) => {
                task.json_output = Some(output.clone());

                for (contract_path, contracts) in output.contracts.into_iter() {
                    let map = self
                        .compiled_contracts
                        .entry(contract_path.clone())
                        .or_default();
                    for (contract_name, contract_info) in contracts.into_iter() {
                        tracing::debug!(
                            contract_path = %contract_path.display(),
                            contract_name = contract_name,
                            "Compiled contract"
                        );

                        map.insert(contract_name, contract_info);
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

    pub fn build_and_publish_libraries(
        &mut self,
        metadata: &Metadata,
        mode: &'a SolcMode,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        self.build_contracts(mode, metadata)?;

        for library_instance in metadata
            .libraries
            .iter()
            .flatten()
            .flat_map(|(_, map)| map.values())
        {
            self.get_or_deploy_contract_instance(
                library_instance,
                metadata,
                None,
                Input::default_caller(),
                None,
                None,
                node,
            )?;
        }

        Ok(())
    }

    pub fn handle_input(
        &mut self,
        metadata: &Metadata,
        case_idx: CaseIdx,
        input: &Input,
        node: &T::Blockchain,
        mode: &SolcMode,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        let deployment_receipts =
            self.handle_contract_deployment(metadata, case_idx, input, node)?;
        let execution_receipt =
            self.handle_input_execution(case_idx, input, deployment_receipts, node)?;
        let tracing_result = self.handle_input_call_frame_tracing(&execution_receipt, node)?;
        self.handle_input_variable_assignment(case_idx, input, &tracing_result)?;
        self.handle_input_expectations(
            case_idx,
            input,
            &execution_receipt,
            node,
            mode,
            &tracing_result,
        )?;
        self.handle_input_diff(case_idx, execution_receipt, node)
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

        let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
        for instance in input.find_all_contract_instances().into_iter() {
            if !self
                .deployed_contracts
                .entry(case_idx)
                .or_insert_with(|| self.deployed_libraries.clone())
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
            let calldata = deploy_with_constructor_arguments.then_some(&input.calldata);
            let value = deploy_with_constructor_arguments
                .then_some(input.value)
                .flatten();

            if let (_, _, Some(receipt)) = self.get_or_deploy_contract_instance(
                &instance,
                metadata,
                case_idx,
                input.caller,
                calldata,
                value,
                node,
            )? {
                receipts.insert(instance.clone(), receipt);
            }
        }

        Ok(receipts)
    }

    /// Handles the execution of the input in terms of the calls that need to be made.
    fn handle_input_execution(
        &mut self,
        case_idx: CaseIdx,
        input: &Input,
        mut deployment_receipts: HashMap<ContractInstance, TransactionReceipt>,
        node: &T::Blockchain,
    ) -> anyhow::Result<TransactionReceipt> {
        match input.method {
            // This input was already executed when `handle_input` was called. We just need to
            // lookup the transaction receipt in this case and continue on.
            Method::Deployer => deployment_receipts
                .remove(&input.instance)
                .context("Failed to find deployment receipt"),
            Method::Fallback | Method::FunctionName(_) => {
                let tx = match input.legacy_transaction(
                    self.deployed_contracts
                        .entry(case_idx)
                        .or_insert_with(|| self.deployed_libraries.clone()),
                    &*self.variables.entry(case_idx).or_default(),
                    node,
                ) {
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
                    Ok(receipt) => Ok(receipt),
                    Err(err) => {
                        tracing::error!(
                            "Failed to execute transaction when executing the contract: {}, {:?}",
                            &*input.instance,
                            err
                        );
                        Err(err)
                    }
                }
            }
        }
    }

    fn handle_input_call_frame_tracing(
        &self,
        execution_receipt: &TransactionReceipt,
        node: &T::Blockchain,
    ) -> anyhow::Result<CallFrame> {
        node.trace_transaction(
            execution_receipt,
            GethDebugTracingOptions {
                tracer: Some(GethDebugTracerType::BuiltInTracer(
                    GethDebugBuiltInTracerType::CallTracer,
                )),
                ..Default::default()
            },
        )
        .map(|trace| {
            trace
                .try_into_call_frame()
                .expect("Impossible - we requested a callframe trace so we must get it back")
        })
    }

    fn handle_input_variable_assignment(
        &mut self,
        case_idx: CaseIdx,
        input: &Input,
        tracing_result: &CallFrame,
    ) -> anyhow::Result<()> {
        let Some(ref assignments) = input.variable_assignments else {
            return Ok(());
        };

        // Handling the return data variable assignments.
        for (variable_name, output_word) in assignments.return_data.iter().zip(
            tracing_result
                .output
                .as_ref()
                .unwrap_or_default()
                .to_vec()
                .chunks(32),
        ) {
            let value = U256::from_be_slice(output_word);
            self.variables
                .entry(case_idx)
                .or_default()
                .insert(variable_name.clone(), value);
        }

        Ok(())
    }

    fn handle_input_expectations(
        &mut self,
        case_idx: CaseIdx,
        input: &Input,
        execution_receipt: &TransactionReceipt,
        node: &T::Blockchain,
        mode: &SolcMode,
        tracing_result: &CallFrame,
    ) -> anyhow::Result<()> {
        let span = tracing::info_span!("Handling input expectations");
        let _guard = span.enter();

        // Resolving the `input.expected` into a series of expectations that we can then assert on.
        let mut expectations = match input {
            Input {
                expected: Some(Expected::Calldata(calldata)),
                ..
            } => vec![ExpectedOutput::new().with_calldata(calldata.clone())],
            Input {
                expected: Some(Expected::Expected(expected)),
                ..
            } => vec![expected.clone()],
            Input {
                expected: Some(Expected::ExpectedMany(expected)),
                ..
            } => expected.clone(),
            Input { expected: None, .. } => vec![ExpectedOutput::new().with_success()],
        };

        // This is a bit of a special case and we have to support it separately on it's own. If it's
        // a call to the deployer method, then the tests will assert that it "returns" the address
        // of the contract. Deployments do not return the address of the contract but the runtime
        // code of the contracts. Therefore, this assertion would always fail. So, we replace it
        // with an assertion of "check if it succeeded"
        if let Method::Deployer = &input.method {
            for expectation in expectations.iter_mut() {
                expectation.return_data = None;
            }
        }

        for expectation in expectations.iter() {
            self.handle_input_expectation_item(
                case_idx,
                execution_receipt,
                node,
                expectation,
                tracing_result,
                mode,
            )?;
        }

        Ok(())
    }

    fn handle_input_expectation_item(
        &mut self,
        case_idx: CaseIdx,
        execution_receipt: &TransactionReceipt,
        node: &T::Blockchain,
        expectation: &ExpectedOutput,
        tracing_result: &CallFrame,
        mode: &SolcMode,
    ) -> anyhow::Result<()> {
        if let Some(ref version_requirement) = expectation.compiler_version {
            let compiler_version = self
                .compiler_version
                .get(mode)
                .context("Failed to find the compiler version fo the solc mode")?;
            if !version_requirement.matches(compiler_version) {
                return Ok(());
            }
        }

        let deployed_contracts = self
            .deployed_contracts
            .entry(case_idx)
            .or_insert_with(|| self.deployed_libraries.clone());
        let variables = self.variables.entry(case_idx).or_default();
        let chain_state_provider = node;

        // Handling the receipt state assertion.
        let expected = !expectation.exception;
        let actual = execution_receipt.status();
        if actual != expected {
            tracing::error!(
                expected,
                actual,
                ?execution_receipt,
                ?tracing_result,
                "Transaction status assertion failed"
            );
            anyhow::bail!(
                "Transaction status assertion failed - Expected {expected} but got {actual}",
            );
        }

        // Handling the calldata assertion
        if let Some(ref expected_calldata) = expectation.return_data {
            let expected = expected_calldata;
            let actual = &tracing_result.output.as_ref().unwrap_or_default();
            if !expected.is_equivalent(
                actual,
                deployed_contracts,
                &*variables,
                chain_state_provider,
            )? {
                tracing::error!(
                    ?execution_receipt,
                    ?expected,
                    %actual,
                    "Calldata assertion failed"
                );
                anyhow::bail!("Calldata assertion failed - Expected {expected:?} but got {actual}",);
            }
        }

        // Handling the events assertion
        if let Some(ref expected_events) = expectation.events {
            // Handling the events length assertion.
            let expected = expected_events.len();
            let actual = execution_receipt.logs().len();
            if actual != expected {
                tracing::error!(expected, actual, "Event count assertion failed",);
                anyhow::bail!(
                    "Event count assertion failed - Expected {expected} but got {actual}",
                );
            }

            // Handling the events assertion.
            for (expected_event, actual_event) in
                expected_events.iter().zip(execution_receipt.logs())
            {
                // Handling the emitter assertion.
                if let Some(ref expected_address) = expected_event.address {
                    let expected = Address::from_slice(
                        Calldata::new_compound([expected_address])
                            .calldata(deployed_contracts, &*variables, node)?
                            .get(12..32)
                            .expect("Can't fail"),
                    );
                    let actual = actual_event.address();
                    if actual != expected {
                        tracing::error!(
                            %expected,
                            %actual,
                            "Event emitter assertion failed",
                        );
                        anyhow::bail!(
                            "Event emitter assertion failed - Expected {expected} but got {actual}",
                        );
                    }
                }

                // Handling the topics assertion.
                for (expected, actual) in expected_event
                    .topics
                    .as_slice()
                    .iter()
                    .zip(actual_event.topics())
                {
                    let expected = Calldata::new_compound([expected]);
                    if !expected.is_equivalent(
                        &actual.0,
                        deployed_contracts,
                        &*variables,
                        chain_state_provider,
                    )? {
                        tracing::error!(
                            ?execution_receipt,
                            ?expected,
                            ?actual,
                            "Event topics assertion failed",
                        );
                        anyhow::bail!(
                            "Event topics assertion failed - Expected {expected:?} but got {actual:?}",
                        );
                    }
                }

                // Handling the values assertion.
                let expected = &expected_event.values;
                let actual = &actual_event.data().data;
                if !expected.is_equivalent(
                    &actual.0,
                    deployed_contracts,
                    &*variables,
                    chain_state_provider,
                )? {
                    tracing::error!(
                        ?execution_receipt,
                        ?expected,
                        ?actual,
                        "Event value assertion failed",
                    );
                    anyhow::bail!(
                        "Event value assertion failed - Expected {expected:?} but got {actual:?}",
                    );
                }
            }
        }

        Ok(())
    }

    fn handle_input_diff(
        &mut self,
        _: CaseIdx,
        execution_receipt: TransactionReceipt,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        let span = tracing::info_span!("Handling input diff");
        let _guard = span.enter();

        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });

        let trace = node.trace_transaction(&execution_receipt, trace_options)?;
        let diff = node.state_diff(&execution_receipt)?;

        Ok((execution_receipt, trace, diff))
    }

    /// Gets the information of a deployed contract or library from the state. If it's found to not
    /// be deployed then it will be deployed.
    ///
    /// If a [`CaseIdx`] is not specified then this contact instance address will be stored in the
    /// cross-case deployed contracts address mapping.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_deploy_contract_instance(
        &mut self,
        contract_instance: &ContractInstance,
        metadata: &Metadata,
        case_idx: impl Into<Option<CaseIdx>>,
        deployer: Address,
        calldata: Option<&Calldata>,
        value: Option<EtherValue>,
        node: &T::Blockchain,
    ) -> anyhow::Result<(Address, JsonAbi, Option<TransactionReceipt>)> {
        let case_idx = case_idx.into();

        let deployed_contracts = match case_idx {
            Some(case_idx) => self
                .deployed_contracts
                .entry(case_idx)
                .or_insert_with(|| self.deployed_libraries.clone()),
            None => &mut self.deployed_libraries,
        };

        if let Some((address, abi)) = deployed_contracts.get(contract_instance) {
            return Ok((*address, abi.clone(), None));
        }

        let Some(ContractPathAndIdent {
            contract_source_path,
            contract_ident,
        }) = metadata.contract_sources()?.remove(contract_instance)
        else {
            tracing::error!("Contract source not found for instance");
            anyhow::bail!(
                "Contract source not found for instance {:?}",
                contract_instance
            )
        };

        let Some((code, abi)) = self
            .compiled_contracts
            .get(&contract_source_path)
            .and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
            .cloned()
        else {
            tracing::error!(
                contract_source_path = contract_source_path.display().to_string(),
                contract_ident = contract_ident.as_ref(),
                "Failed to find information for contract"
            );
            anyhow::bail!(
                "Failed to find information for contract {:?}",
                contract_instance
            )
        };

        let mut code = match alloy::hex::decode(&code) {
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

        if let Some(calldata) = calldata {
            let calldata = calldata.calldata(deployed_contracts, None, node)?;
            code.extend(calldata);
        }

        let tx = {
            let tx = TransactionRequest::default().from(deployer);
            let tx = match value {
                Some(ref value) => tx.value(value.into_inner()),
                _ => tx,
            };
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
            instance_name = ?contract_instance,
            instance_address = ?address,
            "Deployed contract"
        );

        deployed_contracts.insert(contract_instance.clone(), (address, abi.clone()));

        Ok((address, abi, Some(receipt)))
    }
}

pub struct CaseDriver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    case: &'a Case,
    case_idx: CaseIdx,
    config: &'a Arguments,
    leader_node: &'a Leader::Blockchain,
    follower_node: &'a Follower::Blockchain,
}

impl<'a, L, F> CaseDriver<'a, L, F>
where
    L: Platform,
    F: Platform,
{
    pub fn new(
        metadata: &'a Metadata,
        case: &'a Case,
        case_idx: impl Into<CaseIdx>,
        config: &'a Arguments,
        leader_node: &'a L::Blockchain,
        follower_node: &'a F::Blockchain,
    ) -> CaseDriver<'a, L, F> {
        Self {
            metadata,
            case,
            case_idx: case_idx.into(),
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

    // A note on this function and the choice of how we handle errors that happen here. This is not
    // a doc comment since it's a comment for the maintainers of this code and not for the users of
    // this code.
    //
    // This function does a few things: it builds the contracts for the various SOLC modes needed.
    // It deploys the contracts to the chain, and it executes the various inputs that are specified
    // for the test cases.
    //
    // In most functions in the codebase, it's fine to just say "If we encounter an error just
    // bubble it up to the caller", but this isn't a good idea to do here and we need an elaborate
    // way to report errors all while being graceful and continuing execution where we can. For
    // example, if one of the inputs of one of the cases fail to execute, then we should not just
    // bubble that error up immediately. Instead, we should note it down and continue to the next
    // case as the next case might succeed.
    //
    // Therefore, this method returns an `ExecutionResult` object, and not just a normal `Result`.
    // This object is fully typed to contain information about what exactly in the execution was a
    // success and what failed.
    //
    // The above then allows us to have better logging and better information in the caller of this
    // function as we have a more detailed view of what worked and what didn't.
    pub fn execute(&mut self, span: Span) -> ExecutionResult {
        // This is the execution result object that all of the execution information will be
        // collected into and returned at the end of the execution.
        let mut execution_result = ExecutionResult::default();

        let tracing_span = tracing::info_span!("Handling metadata file");
        let _guard = tracing_span.enter();

        // We only execute this input if it's valid for the leader and the follower. Otherwise, we
        // skip it with a warning.
        if !self
            .leader_node
            .matches_target(self.metadata.targets.as_deref())
            || !self
                .follower_node
                .matches_target(self.metadata.targets.as_deref())
        {
            tracing::warn!(
                targets = ?self.metadata.targets,
                "Either the leader or follower node do not support the targets of the file"
            );
            return execution_result;
        }

        for mode in self.metadata.solc_modes() {
            let tracing_span = tracing::info_span!("With solc mode", solc_mode = ?mode);
            let _guard = tracing_span.enter();

            let mut leader_state = CaseState::<L>::new(self.config, span);
            let mut follower_state = CaseState::<F>::new(self.config, span);

            // Note: we are currently forced to do two compilation passes due to linking. In the
            // first compilation pass we compile the libraries and publish them to the chain. In the
            // second compilation pass we compile the contracts with the library addresses so that
            // they're linked at compile-time.
            let build_result = tracing::info_span!("Building and publishing libraries")
                .in_scope(|| {
                    match leader_state.build_and_publish_libraries(self.metadata, &mode, self.leader_node) {
                        Ok(_) => {
                            tracing::debug!(target = ?Target::Leader, "Library building succeeded");
                            execution_result.add_successful_build(Target::Leader, mode.clone());
                        },
                        Err(error) => {
                            tracing::error!(target = ?Target::Leader, ?error, "Library building failed");
                            execution_result.add_failed_build(Target::Leader, mode.clone(), error);
                            return Err(());
                        }
                    }
                    match follower_state.build_and_publish_libraries(self.metadata, &mode, self.follower_node) {
                        Ok(_) => {
                            tracing::debug!(target = ?Target::Follower, "Library building succeeded");
                            execution_result.add_successful_build(Target::Follower, mode.clone());
                        },
                        Err(error) => {
                            tracing::error!(target = ?Target::Follower, ?error, "Library building failed");
                            execution_result.add_failed_build(Target::Follower, mode.clone(), error);
                            return Err(());
                        }
                    }
                    Ok(())
                });
            if build_result.is_err() {
                // Note: We skip to the next solc mode as there's nothing that we can do at this
                // point, the building has failed. We do NOT bail out of the execution as a whole.
                continue;
            }

            // We build the contracts. If building the contracts for the metadata file fails then we
            // have no other option but to keep note of this error and move on to the next solc mode
            // and NOT just bail out of the execution as a whole.
            let build_result = tracing::info_span!("Building contracts").in_scope(|| {
                match leader_state.build_contracts(&mode, self.metadata) {
                    Ok(_) => {
                        tracing::debug!(target = ?Target::Leader, "Contract building succeeded");
                        execution_result.add_successful_build(Target::Leader, mode.clone());
                    },
                    Err(error) => {
                        tracing::error!(target = ?Target::Leader, ?error, "Contract building failed");
                        execution_result.add_failed_build(Target::Leader, mode.clone(), error);
                        return Err(());
                    }
                }
                match follower_state.build_contracts(&mode, self.metadata) {
                    Ok(_) => {
                        tracing::debug!(target = ?Target::Follower, "Contract building succeeded");
                        execution_result.add_successful_build(Target::Follower, mode.clone());
                    },
                    Err(error) => {
                        tracing::error!(target = ?Target::Follower, ?error, "Contract building failed");
                        execution_result.add_failed_build(Target::Follower, mode.clone(), error);
                        return Err(());
                    }
                }
                Ok(())
            });
            if build_result.is_err() {
                // Note: We skip to the next solc mode as there's nothing that we can do at this
                // point, the building has failed. We do NOT bail out of the execution as a whole.
                continue;
            }

            // For cases if one of the inputs fail then we move on to the next case and we do NOT
            // bail out of the whole thing.
            let case = self.case;
            let case_idx = self.case_idx;
            let tracing_span =
                tracing::info_span!("Handling case", case_name = case.name, case_idx = *case_idx);
            let _guard = tracing_span.enter();

            let case_idx = CaseIdx::new(case_idx);

            // For inputs if one of the inputs fail we move on to the next case (we do not move
            // on to the next input as it doesn't make sense. It depends on the previous one).
            for (input_idx, input) in case.inputs_iterator().enumerate() {
                let tracing_span = tracing::info_span!("Handling input", input_idx);
                let _guard = tracing_span.enter();

                let input_execution_result =
                    tracing::info_span!("Executing input", contract_name = ?input.instance)
                        .in_scope(|| {
                            let (leader_receipt, _, leader_diff) = match leader_state.handle_input(
                                self.metadata,
                                case_idx,
                                &input,
                                self.leader_node,
                                &mode,
                            ) {
                                Ok(result) => result,
                                Err(error) => {
                                    tracing::error!(
                                        target = ?Target::Leader,
                                        ?error,
                                        "Contract execution failed"
                                    );
                                    execution_result.add_failed_case(
                                        Target::Leader,
                                        mode.clone(),
                                        case.name.as_deref().unwrap_or("no case name").to_owned(),
                                        case_idx,
                                        input_idx,
                                        anyhow::Error::msg(format!("{error}")),
                                    );
                                    return Err(error);
                                }
                            };

                            let (follower_receipt, _, follower_diff) = match follower_state
                                .handle_input(
                                    self.metadata,
                                    case_idx,
                                    &input,
                                    self.follower_node,
                                    &mode,
                                ) {
                                Ok(result) => result,
                                Err(error) => {
                                    tracing::error!(
                                        target = ?Target::Follower,
                                        ?error,
                                        "Contract execution failed"
                                    );
                                    execution_result.add_failed_case(
                                        Target::Follower,
                                        mode.clone(),
                                        case.name.as_deref().unwrap_or("no case name").to_owned(),
                                        case_idx,
                                        input_idx,
                                        anyhow::Error::msg(format!("{error}")),
                                    );
                                    return Err(error);
                                }
                            };

                            Ok((leader_receipt, leader_diff, follower_receipt, follower_diff))
                        });
                let Ok((leader_receipt, leader_diff, follower_receipt, follower_diff)) =
                    input_execution_result
                else {
                    return execution_result;
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
            }

            // Note: Only consider the case as having been successful after we have processed
            // all of the inputs and completed the entire loop over the input.
            execution_result.add_successful_case(
                Target::Leader,
                mode.clone(),
                case.name.clone().unwrap_or("no case name".to_owned()),
                case_idx,
            );
            execution_result.add_successful_case(
                Target::Follower,
                mode.clone(),
                case.name.clone().unwrap_or("no case name".to_owned()),
                case_idx,
            );
        }

        execution_result
    }
}

#[derive(Debug, Default)]
pub struct ExecutionResult {
    pub results: Vec<Box<dyn ExecutionResultItem>>,
    pub successful_cases_count: usize,
    pub failed_cases_count: usize,
}

impl ExecutionResult {
    pub fn new() -> Self {
        Self {
            results: Default::default(),
            successful_cases_count: Default::default(),
            failed_cases_count: Default::default(),
        }
    }

    pub fn add_successful_build(&mut self, target: Target, solc_mode: SolcMode) {
        self.results
            .push(Box::new(BuildResult::Success { target, solc_mode }));
    }

    pub fn add_failed_build(&mut self, target: Target, solc_mode: SolcMode, error: anyhow::Error) {
        self.results.push(Box::new(BuildResult::Failure {
            target,
            solc_mode,
            error,
        }));
    }

    pub fn add_successful_case(
        &mut self,
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: CaseIdx,
    ) {
        self.successful_cases_count += 1;
        self.results.push(Box::new(CaseResult::Success {
            target,
            solc_mode,
            case_name,
            case_idx,
        }));
    }

    pub fn add_failed_case(
        &mut self,
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: CaseIdx,
        input_idx: usize,
        error: anyhow::Error,
    ) {
        self.failed_cases_count += 1;
        self.results.push(Box::new(CaseResult::Failure {
            target,
            solc_mode,
            case_name,
            case_idx,
            error,
            input_idx,
        }));
    }
}

pub trait ExecutionResultItem: Debug {
    /// Converts this result item into an [`anyhow::Result`].
    fn as_result(&self) -> Result<(), &anyhow::Error>;

    /// Provides information on whether the provided result item is of a success or failure.
    fn is_success(&self) -> bool;

    /// Provides information of the target that this result is for.
    fn target(&self) -> &Target;

    /// Provides information on the [`SolcMode`] mode that we being used for this result item.
    fn solc_mode(&self) -> &SolcMode;

    /// Provides information on the case name and number that this result item pertains to. This is
    /// [`None`] if the error doesn't belong to any case (e.g., if it's a build error outside of any
    /// of the cases.).
    fn case_name_and_index(&self) -> Option<(&str, &CaseIdx)>;

    /// Provides information on the input number that this result item pertains to. This is [`None`]
    /// if the error doesn't belong to any input (e.g., if it's a build error outside of any of the
    /// inputs.).
    fn input_index(&self) -> Option<usize>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Leader,
    Follower,
}

#[derive(Debug)]
pub enum BuildResult {
    Success {
        target: Target,
        solc_mode: SolcMode,
    },
    Failure {
        target: Target,
        solc_mode: SolcMode,
        error: anyhow::Error,
    },
}

impl ExecutionResultItem for BuildResult {
    fn as_result(&self) -> Result<(), &anyhow::Error> {
        match self {
            Self::Success { .. } => Ok(()),
            Self::Failure { error, .. } => Err(error)?,
        }
    }

    fn is_success(&self) -> bool {
        match self {
            Self::Success { .. } => true,
            Self::Failure { .. } => false,
        }
    }

    fn target(&self) -> &Target {
        match self {
            Self::Success { target, .. } | Self::Failure { target, .. } => target,
        }
    }

    fn solc_mode(&self) -> &SolcMode {
        match self {
            Self::Success { solc_mode, .. } | Self::Failure { solc_mode, .. } => solc_mode,
        }
    }

    fn case_name_and_index(&self) -> Option<(&str, &CaseIdx)> {
        None
    }

    fn input_index(&self) -> Option<usize> {
        None
    }
}

#[derive(Debug)]
pub enum CaseResult {
    Success {
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: CaseIdx,
    },
    Failure {
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: CaseIdx,
        input_idx: usize,
        error: anyhow::Error,
    },
}

impl ExecutionResultItem for CaseResult {
    fn as_result(&self) -> Result<(), &anyhow::Error> {
        match self {
            Self::Success { .. } => Ok(()),
            Self::Failure { error, .. } => Err(error)?,
        }
    }

    fn is_success(&self) -> bool {
        match self {
            Self::Success { .. } => true,
            Self::Failure { .. } => false,
        }
    }

    fn target(&self) -> &Target {
        match self {
            Self::Success { target, .. } | Self::Failure { target, .. } => target,
        }
    }

    fn solc_mode(&self) -> &SolcMode {
        match self {
            Self::Success { solc_mode, .. } | Self::Failure { solc_mode, .. } => solc_mode,
        }
    }

    fn case_name_and_index(&self) -> Option<(&str, &CaseIdx)> {
        match self {
            Self::Success {
                case_name,
                case_idx,
                ..
            }
            | Self::Failure {
                case_name,
                case_idx,
                ..
            } => Some((case_name, case_idx)),
        }
    }

    fn input_index(&self) -> Option<usize> {
        match self {
            CaseResult::Success { .. } => None,
            CaseResult::Failure { input_idx, .. } => Some(*input_idx),
        }
    }
}
