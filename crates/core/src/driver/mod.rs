//! The test driver handles the compilation and execution of the test cases.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::PathBuf;

use alloy::consensus::EMPTY_ROOT_HASH;
use alloy::hex;
use alloy::json_abi::JsonAbi;
use alloy::network::{Ethereum, TransactionBuilder};
use alloy::primitives::U256;
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::{
    CallFrame, GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType,
    GethDebugTracingOptions, GethTrace, PreStateConfig,
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
use revive_dt_format::traits::{ResolutionContext, ResolverApi};
use semver::Version;

use revive_dt_format::case::{Case, CaseIdx};
use revive_dt_format::input::{
    BalanceAssertion, Calldata, EtherValue, Expected, ExpectedOutput, Input, Method,
    StorageEmptyAssertion,
};
use revive_dt_format::metadata::{ContractIdent, ContractInstance, ContractPathAndIdent};
use revive_dt_format::{input::Step, metadata::Metadata};
use revive_dt_node::Node;
use revive_dt_node_interaction::EthereumNode;
use tracing::Instrument;

use crate::Platform;

pub struct CaseState<T: Platform> {
    /// A map of all of the compiled contracts for the given metadata file.
    compiled_contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,

    /// This map stores the contracts deployments for this case.
    deployed_contracts: HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,

    /// This map stores the variables used for each one of the cases contained in the metadata
    /// file.
    variables: HashMap<String, U256>,

    /// Stores the version used for the current case.
    compiler_version: Version,

    phantom: PhantomData<T>,
}

impl<T> CaseState<T>
where
    T: Platform,
{
    pub fn new(
        compiler_version: Version,
        compiled_contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,
        deployed_contracts: HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,
    ) -> Self {
        Self {
            compiled_contracts,
            deployed_contracts,
            variables: Default::default(),
            compiler_version,
            phantom: PhantomData,
        }
    }

    pub async fn handle_step(
        &mut self,
        metadata: &Metadata,
        case_idx: CaseIdx,
        step: &Step,
        node: &T::Blockchain,
    ) -> anyhow::Result<StepOutput> {
        match step {
            Step::FunctionCall(input) => {
                let (receipt, geth_trace, diff_mode) =
                    self.handle_input(metadata, case_idx, input, node).await?;
                Ok(StepOutput::FunctionCall(receipt, geth_trace, diff_mode))
            }
            Step::BalanceAssertion(balance_assertion) => {
                self.handle_balance_assertion(metadata, case_idx, balance_assertion, node)
                    .await?;
                Ok(StepOutput::BalanceAssertion)
            }
            Step::StorageEmptyAssertion(storage_empty) => {
                self.handle_storage_empty(metadata, case_idx, storage_empty, node)
                    .await?;
                Ok(StepOutput::StorageEmptyAssertion)
            }
        }
    }

    pub async fn handle_input(
        &mut self,
        metadata: &Metadata,
        case_idx: CaseIdx,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        let deployment_receipts = self
            .handle_input_contract_deployment(metadata, case_idx, input, node)
            .await?;
        let execution_receipt = self
            .handle_input_execution(input, deployment_receipts, node)
            .await?;
        let tracing_result = self
            .handle_input_call_frame_tracing(&execution_receipt, node)
            .await?;
        self.handle_input_variable_assignment(input, &tracing_result)?;
        self.handle_input_expectations(input, &execution_receipt, node, &tracing_result)
            .await?;
        self.handle_input_diff(case_idx, execution_receipt, node)
            .await
    }

    pub async fn handle_balance_assertion(
        &mut self,
        metadata: &Metadata,
        _: CaseIdx,
        balance_assertion: &BalanceAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        self.handle_balance_assertion_contract_deployment(metadata, balance_assertion, node)
            .await?;
        self.handle_balance_assertion_execution(balance_assertion, node)
            .await?;
        Ok(())
    }

    pub async fn handle_storage_empty(
        &mut self,
        metadata: &Metadata,
        _: CaseIdx,
        storage_empty: &StorageEmptyAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        self.handle_storage_empty_assertion_contract_deployment(metadata, storage_empty, node)
            .await?;
        self.handle_storage_empty_assertion_execution(storage_empty, node)
            .await?;
        Ok(())
    }

    /// Handles the contract deployment for a given input performing it if it needs to be performed.
    async fn handle_input_contract_deployment(
        &mut self,
        metadata: &Metadata,
        _: CaseIdx,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<HashMap<ContractInstance, TransactionReceipt>> {
        let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
        for instance in input.find_all_contract_instances().into_iter() {
            if !self.deployed_contracts.contains_key(&instance) {
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

            if let (_, _, Some(receipt)) = self
                .get_or_deploy_contract_instance(
                    &instance,
                    metadata,
                    input.caller,
                    calldata,
                    value,
                    node,
                )
                .await?
            {
                receipts.insert(instance.clone(), receipt);
            }
        }

        Ok(receipts)
    }

    /// Handles the execution of the input in terms of the calls that need to be made.
    async fn handle_input_execution(
        &mut self,
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
                let tx = match input
                    .legacy_transaction(node, self.default_resolution_context())
                    .await
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

                match node.execute_transaction(tx).await {
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

    async fn handle_input_call_frame_tracing(
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
                tracer_config: GethDebugTracerConfig(serde_json::json! {{
                    "onlyTopCall": true,
                    "withLog": false,
                    "withStorage": false,
                    "withMemory": false,
                    "withStack": false,
                    "withReturnData": true
                }}),
                ..Default::default()
            },
        )
        .await
        .map(|trace| {
            trace
                .try_into_call_frame()
                .expect("Impossible - we requested a callframe trace so we must get it back")
        })
    }

    fn handle_input_variable_assignment(
        &mut self,
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
            self.variables.insert(variable_name.clone(), value);
            tracing::info!(
                variable_name,
                variable_value = hex::encode(value.to_be_bytes::<32>()),
                "Assigned variable"
            );
        }

        Ok(())
    }

    async fn handle_input_expectations(
        &mut self,
        input: &Input,
        execution_receipt: &TransactionReceipt,
        resolver: &impl ResolverApi,
        tracing_result: &CallFrame,
    ) -> anyhow::Result<()> {
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
                execution_receipt,
                resolver,
                expectation,
                tracing_result,
            )
            .await?;
        }

        Ok(())
    }

    async fn handle_input_expectation_item(
        &mut self,
        execution_receipt: &TransactionReceipt,
        resolver: &impl ResolverApi,
        expectation: &ExpectedOutput,
        tracing_result: &CallFrame,
    ) -> anyhow::Result<()> {
        if let Some(ref version_requirement) = expectation.compiler_version {
            if !version_requirement.matches(&self.compiler_version) {
                return Ok(());
            }
        }

        let resolution_context = self
            .default_resolution_context()
            .with_block_number(execution_receipt.block_number.as_ref())
            .with_transaction_hash(&execution_receipt.transaction_hash);

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
            if !expected
                .is_equivalent(actual, resolver, resolution_context)
                .await?
            {
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
            for (event_idx, (expected_event, actual_event)) in expected_events
                .iter()
                .zip(execution_receipt.logs())
                .enumerate()
            {
                // Handling the emitter assertion.
                if let Some(ref expected_address) = expected_event.address {
                    let expected = Address::from_slice(
                        Calldata::new_compound([expected_address])
                            .calldata(resolver, resolution_context)
                            .await?
                            .get(12..32)
                            .expect("Can't fail"),
                    );
                    let actual = actual_event.address();
                    if actual != expected {
                        tracing::error!(
                            event_idx,
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
                    if !expected
                        .is_equivalent(&actual.0, resolver, resolution_context)
                        .await?
                    {
                        tracing::error!(
                            event_idx,
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
                if !expected
                    .is_equivalent(&actual.0, resolver, resolution_context)
                    .await?
                {
                    tracing::error!(
                        event_idx,
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

    async fn handle_input_diff(
        &mut self,
        _: CaseIdx,
        execution_receipt: TransactionReceipt,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });

        let trace = node
            .trace_transaction(&execution_receipt, trace_options)
            .await?;
        let diff = node.state_diff(&execution_receipt).await?;

        Ok((execution_receipt, trace, diff))
    }

    pub async fn handle_balance_assertion_contract_deployment(
        &mut self,
        metadata: &Metadata,
        balance_assertion: &BalanceAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        let Some(instance) = balance_assertion
            .address
            .strip_suffix(".address")
            .map(ContractInstance::new)
        else {
            return Ok(());
        };
        self.get_or_deploy_contract_instance(
            &instance,
            metadata,
            Input::default_caller(),
            None,
            None,
            node,
        )
        .await?;
        Ok(())
    }

    pub async fn handle_balance_assertion_execution(
        &mut self,
        BalanceAssertion {
            address: address_string,
            expected_balance: amount,
            ..
        }: &BalanceAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        let address = Address::from_slice(
            Calldata::new_compound([address_string])
                .calldata(node, self.default_resolution_context())
                .await?
                .get(12..32)
                .expect("Can't fail"),
        );

        let balance = node.balance_of(address).await?;

        let expected = *amount;
        let actual = balance;
        if expected != actual {
            tracing::error!(%expected, %actual, %address, "Balance assertion failed");
            anyhow::bail!(
                "Balance assertion failed - Expected {} but got {} for {} resolved to {}",
                expected,
                actual,
                address_string,
                address,
            )
        }

        Ok(())
    }

    pub async fn handle_storage_empty_assertion_contract_deployment(
        &mut self,
        metadata: &Metadata,
        storage_empty_assertion: &StorageEmptyAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        let Some(instance) = storage_empty_assertion
            .address
            .strip_suffix(".address")
            .map(ContractInstance::new)
        else {
            return Ok(());
        };
        self.get_or_deploy_contract_instance(
            &instance,
            metadata,
            Input::default_caller(),
            None,
            None,
            node,
        )
        .await?;
        Ok(())
    }

    pub async fn handle_storage_empty_assertion_execution(
        &mut self,
        StorageEmptyAssertion {
            address: address_string,
            is_storage_empty,
            ..
        }: &StorageEmptyAssertion,
        node: &T::Blockchain,
    ) -> anyhow::Result<()> {
        let address = Address::from_slice(
            Calldata::new_compound([address_string])
                .calldata(node, self.default_resolution_context())
                .await?
                .get(12..32)
                .expect("Can't fail"),
        );

        let storage = node.latest_state_proof(address, Default::default()).await?;
        let is_empty = storage.storage_hash == EMPTY_ROOT_HASH;

        let expected = is_storage_empty;
        let actual = is_empty;

        if *expected != actual {
            tracing::error!(%expected, %actual, %address, "Storage Empty Assertion failed");
            anyhow::bail!(
                "Storage Empty Assertion failed - Expected {} but got {} for {} resolved to {}",
                expected,
                actual,
                address_string,
                address,
            )
        };

        Ok(())
    }

    /// Gets the information of a deployed contract or library from the state. If it's found to not
    /// be deployed then it will be deployed.
    ///
    /// If a [`CaseIdx`] is not specified then this contact instance address will be stored in the
    /// cross-case deployed contracts address mapping.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_or_deploy_contract_instance(
        &mut self,
        contract_instance: &ContractInstance,
        metadata: &Metadata,
        deployer: Address,
        calldata: Option<&Calldata>,
        value: Option<EtherValue>,
        node: &T::Blockchain,
    ) -> anyhow::Result<(Address, JsonAbi, Option<TransactionReceipt>)> {
        if let Some((_, address, abi)) = self.deployed_contracts.get(contract_instance) {
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
            let calldata = calldata
                .calldata(node, self.default_resolution_context())
                .await?;
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

        let receipt = match node.execute_transaction(tx).await {
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

        self.deployed_contracts.insert(
            contract_instance.clone(),
            (contract_ident, address, abi.clone()),
        );

        Ok((address, abi, Some(receipt)))
    }

    fn default_resolution_context(&self) -> ResolutionContext<'_> {
        ResolutionContext::default()
            .with_deployed_contracts(&self.deployed_contracts)
            .with_variables(&self.variables)
    }
}

pub struct CaseDriver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    case: &'a Case,
    case_idx: CaseIdx,
    leader_node: &'a Leader::Blockchain,
    follower_node: &'a Follower::Blockchain,
    leader_state: CaseState<Leader>,
    follower_state: CaseState<Follower>,
}

impl<'a, L, F> CaseDriver<'a, L, F>
where
    L: Platform,
    F: Platform,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        metadata: &'a Metadata,
        case: &'a Case,
        case_idx: impl Into<CaseIdx>,
        leader_node: &'a L::Blockchain,
        follower_node: &'a F::Blockchain,
        leader_state: CaseState<L>,
        follower_state: CaseState<F>,
    ) -> CaseDriver<'a, L, F> {
        Self {
            metadata,
            case,
            case_idx: case_idx.into(),
            leader_node,
            follower_node,
            leader_state,
            follower_state,
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

    pub async fn execute(&mut self) -> anyhow::Result<usize> {
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
            return Ok(0);
        }

        let mut steps_executed = 0;
        for (step_idx, step) in self.case.steps_iterator().enumerate() {
            let tracing_span = tracing::info_span!("Handling input", step_idx);

            let leader_step_output = self
                .leader_state
                .handle_step(self.metadata, self.case_idx, &step, self.leader_node)
                .instrument(tracing_span.clone())
                .await?;
            let follower_step_output = self
                .follower_state
                .handle_step(self.metadata, self.case_idx, &step, self.follower_node)
                .instrument(tracing_span)
                .await?;
            match (leader_step_output, follower_step_output) {
                (
                    StepOutput::FunctionCall(leader_receipt, _, leader_diff),
                    StepOutput::FunctionCall(follower_receipt, _, follower_diff),
                ) => {
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
                (StepOutput::BalanceAssertion, StepOutput::BalanceAssertion) => {}
                (StepOutput::StorageEmptyAssertion, StepOutput::StorageEmptyAssertion) => {}
                _ => unreachable!("The two step outputs can not be of a different kind"),
            }

            steps_executed += 1;
        }

        Ok(steps_executed)
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum StepOutput {
    FunctionCall(TransactionReceipt, GethTrace, DiffMode),
    BalanceAssertion,
    StorageEmptyAssertion,
}
