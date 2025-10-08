use std::{
	collections::{BTreeMap, HashMap},
	sync::Arc,
};

use alloy::{
	consensus::EMPTY_ROOT_HASH,
	hex,
	json_abi::JsonAbi,
	network::{Ethereum, TransactionBuilder},
	primitives::{Address, TxHash, U256},
	rpc::types::{
		TransactionReceipt, TransactionRequest,
		trace::geth::{
			CallFrame, GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType,
			GethDebugTracingOptions,
		},
	},
};
use anyhow::{Context as _, Result, bail};
use futures::TryStreamExt;
use indexmap::IndexMap;
use revive_dt_common::types::{PlatformIdentifier, PrivateKeyAllocator};
use revive_dt_format::{
	metadata::{ContractInstance, ContractPathAndIdent},
	steps::{
		AllocateAccountStep, BalanceAssertionStep, Calldata, EtherValue, Expected, ExpectedOutput,
		FunctionCallStep, Method, RepeatStep, Step, StepAddress, StepIdx, StepPath,
		StorageEmptyAssertionStep,
	},
	traits::ResolutionContext,
};
use tokio::sync::Mutex;
use tracing::{error, info, instrument};

use crate::{
	differential_tests::ExecutionState,
	helpers::{CachedCompiler, TestDefinition, TestPlatformInformation},
};

type StepsIterator = std::vec::IntoIter<(StepPath, Step)>;

pub struct Driver<'a, I> {
	/// The drivers for the various platforms that we're executing the tests on.
	platform_drivers: BTreeMap<PlatformIdentifier, PlatformDriver<'a, I>>,
}

impl<'a, I> Driver<'a, I> where I: Iterator<Item = (StepPath, Step)> {}

impl<'a> Driver<'a, StepsIterator> {
	// region:Constructors
	pub async fn new_root(
		test_definition: &'a TestDefinition<'a>,
		private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
		cached_compiler: &CachedCompiler<'a>,
	) -> Result<Self> {
		let platform_drivers = futures::future::try_join_all(test_definition.platforms.iter().map(
			|(identifier, information)| {
				let identifier = *identifier;
				let private_key_allocator = private_key_allocator.clone();
				async move {
					Self::create_platform_driver(
						identifier,
						information,
						test_definition,
						private_key_allocator,
						cached_compiler,
					)
					.await
					.map(|driver| (identifier, driver))
				}
			},
		))
		.await
		.context("Failed to create the drivers for the various platforms")?
		.into_iter()
		.collect::<BTreeMap<_, _>>();

		Ok(Self { platform_drivers })
	}

	async fn create_platform_driver(
		identifier: PlatformIdentifier,
		information: &'a TestPlatformInformation<'a>,
		test_definition: &'a TestDefinition<'a>,
		private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
		cached_compiler: &CachedCompiler<'a>,
	) -> Result<PlatformDriver<'a, StepsIterator>> {
		let steps: Vec<(StepPath, Step)> = test_definition
			.case
			.steps_iterator()
			.enumerate()
			.map(|(step_idx, step)| -> (StepPath, Step) {
				(StepPath::new(vec![StepIdx::new(step_idx)]), step)
			})
			.collect();
		let steps_iterator: StepsIterator = steps.into_iter();

		PlatformDriver::new(
			information,
			test_definition,
			private_key_allocator,
			cached_compiler,
			steps_iterator,
		)
		.await
		.context(format!("Failed to create driver for {identifier}"))
	}
	// endregion:Constructors

	// region:Execution
	pub async fn execute_all(mut self) -> Result<usize> {
		let platform_drivers = std::mem::take(&mut self.platform_drivers);
		let results = futures::future::try_join_all(
			platform_drivers.into_values().map(|driver| driver.execute_all()),
		)
		.await
		.context("Failed to execute all of the steps on the driver")?;
		Ok(results.first().copied().unwrap_or_default())
	}
	// endregion:Execution
}

/// The differential tests driver for a single platform.
pub struct PlatformDriver<'a, I> {
	/// The information of the platform that this driver is for.
	platform_information: &'a TestPlatformInformation<'a>,

	/// The definition of the test that the driver is instructed to execute.
	test_definition: &'a TestDefinition<'a>,

	/// The private key allocator used by this driver and other drivers when account allocations
	/// are needed.
	private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,

	/// The execution state associated with the platform.
	execution_state: ExecutionState,

	/// The number of steps that were executed on the driver.
	steps_executed: usize,

	/// This is the queue of steps that are to be executed by the driver for this test case. Each
	/// time `execute_step` is called one of the steps is executed.
	steps_iterator: I,
}

impl<'a, I> PlatformDriver<'a, I>
where
	I: Iterator<Item = (StepPath, Step)>,
{
	// region:Constructors & Initialization

	pub async fn new(
		platform_information: &'a TestPlatformInformation<'a>,
		test_definition: &'a TestDefinition<'a>,
		private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
		cached_compiler: &CachedCompiler<'a>,
		steps: I,
	) -> Result<Self> {
		let execution_state =
			Self::init_execution_state(platform_information, test_definition, cached_compiler)
				.await
				.context("Failed to initialize the execution state of the platform")?;
		Ok(PlatformDriver {
			platform_information,
			test_definition,
			private_key_allocator,
			execution_state,
			steps_executed: 0,
			steps_iterator: steps,
		})
	}

	async fn init_execution_state(
		platform_information: &'a TestPlatformInformation<'a>,
		test_definition: &'a TestDefinition<'a>,
		cached_compiler: &CachedCompiler<'a>,
	) -> Result<ExecutionState> {
		let compiler_output = cached_compiler
			.compile_contracts(
				test_definition.metadata,
				test_definition.metadata_file_path,
				test_definition.mode.clone(),
				None,
				platform_information.compiler.as_ref(),
				platform_information.platform,
				&platform_information.reporter,
			)
			.await
			.inspect_err(|err| {
				error!(
					?err,
					platform_identifier = %platform_information.platform.platform_identifier(),
					"Pre-linking compilation failed"
				)
			})
			.context("Failed to produce the pre-linking compiled contracts")?;

		let mut deployed_libraries = None::<HashMap<_, _>>;
		let mut contract_sources = test_definition
			.metadata
			.contract_sources()
			.inspect_err(|err| {
				error!(
					?err,
					platform_identifier = %platform_information.platform.platform_identifier(),
					"Failed to retrieve contract sources from metadata"
				)
			})
			.context("Failed to get the contract instances from the metadata file")?;
		for library_instance in test_definition
			.metadata
			.libraries
			.iter()
			.flatten()
			.flat_map(|(_, map)| map.values())
		{
			let ContractPathAndIdent {
				contract_source_path: library_source_path,
				contract_ident: library_ident,
			} = contract_sources
				.remove(library_instance)
				.context("Failed to get the contract sources of the contract instance")?;

			let (code, abi) = compiler_output
				.contracts
				.get(&library_source_path)
				.and_then(|contracts| contracts.get(library_ident.as_str()))
				.context("Failed to get the code and abi for the instance")?;

			let code = alloy::hex::decode(code)?;

			// Getting the deployer address from the cases themselves. This is to ensure
			// that we're doing the deployments from different accounts and therefore we're
			// not slowed down by the nonce.
			let deployer_address = test_definition
				.case
				.steps
				.iter()
				.filter_map(|step| match step {
					Step::FunctionCall(input) => input.caller.as_address().copied(),
					Step::BalanceAssertion(..) => None,
					Step::StorageEmptyAssertion(..) => None,
					Step::Repeat(..) => None,
					Step::AllocateAccount(..) => None,
				})
				.next()
				.unwrap_or(FunctionCallStep::default_caller_address());
			let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
				TransactionRequest::default().from(deployer_address),
				code,
			);
			let receipt =
				platform_information.node.execute_transaction(tx).await.inspect_err(|err| {
					error!(
						?err,
						%library_instance,
						platform_identifier = %platform_information.platform.platform_identifier(),
						"Failed to deploy the library"
					)
				})?;

			let library_address = receipt.contract_address.expect("Failed to deploy the library");

			deployed_libraries.get_or_insert_default().insert(
				library_instance.clone(),
				(library_ident.clone(), library_address, abi.clone()),
			);
		}

		let compiler_output = cached_compiler
			.compile_contracts(
				test_definition.metadata,
				test_definition.metadata_file_path,
				test_definition.mode.clone(),
				deployed_libraries.as_ref(),
				platform_information.compiler.as_ref(),
				platform_information.platform,
				&platform_information.reporter,
			)
			.await
			.inspect_err(|err| {
				error!(
					?err,
					platform_identifier = %platform_information.platform.platform_identifier(),
					"Pre-linking compilation failed"
				)
			})
			.context("Failed to compile the post-link contracts")?;

		Ok(ExecutionState::new(compiler_output.contracts, deployed_libraries.unwrap_or_default()))
	}
	// endregion:Constructors & Initialization

	// region:Step Handling
	pub async fn execute_all(mut self) -> Result<usize> {
		while let Some(result) = self.execute_next_step().await {
			result?
		}
		Ok(self.steps_executed)
	}

	pub async fn execute_next_step(&mut self) -> Option<Result<()>> {
		let (step_path, step) = self.steps_iterator.next()?;
		info!(%step_path, "Executing Step");
		Some(
			self.execute_step(&step_path, &step)
				.await
				.inspect(|_| info!(%step_path, "Step execution succeeded"))
				.inspect_err(|err| error!(%step_path, ?err, "Step execution failed")),
		)
	}

	#[instrument(
        level = "info",
        skip_all,
        fields(
            platform_identifier = %self.platform_information.platform.platform_identifier(),
            node_id = self.platform_information.node.id(),
            %step_path,
        ),
        err(Debug),
    )]
	async fn execute_step(&mut self, step_path: &StepPath, step: &Step) -> Result<()> {
		let steps_executed = match step {
			Step::FunctionCall(step) => self
				.execute_function_call(step_path, step.as_ref())
				.await
				.context("Function call step Failed"),
			Step::BalanceAssertion(step) => self
				.execute_balance_assertion(step_path, step.as_ref())
				.await
				.context("Balance Assertion Step Failed"),
			Step::StorageEmptyAssertion(step) => self
				.execute_storage_empty_assertion_step(step_path, step.as_ref())
				.await
				.context("Storage Empty Assertion Step Failed"),
			Step::Repeat(step) => self
				.execute_repeat_step(step_path, step.as_ref())
				.await
				.context("Repetition Step Failed"),
			Step::AllocateAccount(step) => self
				.execute_account_allocation(step_path, step.as_ref())
				.await
				.context("Account Allocation Step Failed"),
		}?;
		self.steps_executed += steps_executed;
		Ok(())
	}

	#[instrument(level = "info", skip_all)]
	pub async fn execute_function_call(
		&mut self,
		_: &StepPath,
		step: &FunctionCallStep,
	) -> Result<usize> {
		let deployment_receipts = self
			.handle_function_call_contract_deployment(step)
			.await
			.context("Failed to deploy contracts for the function call step")?;
		let execution_receipt = self
			.handle_function_call_execution(step, deployment_receipts)
			.await
			.context("Failed to handle the function call execution")?;
		let tracing_result = self
			.handle_function_call_call_frame_tracing(execution_receipt.transaction_hash)
			.await
			.context("Failed to handle the function call call frame tracing")?;
		self.handle_function_call_variable_assignment(step, &tracing_result)
			.await
			.context("Failed to handle function call variable assignment")?;
		self.handle_function_call_assertions(step, &execution_receipt, &tracing_result)
			.await
			.context("Failed to handle function call assertions")?;
		Ok(1)
	}

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_contract_deployment(
		&mut self,
		step: &FunctionCallStep,
	) -> Result<HashMap<ContractInstance, TransactionReceipt>> {
		let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
		for instance in step.find_all_contract_instances().into_iter() {
			if !self.execution_state.deployed_contracts.contains_key(&instance) {
				instances_we_must_deploy.entry(instance).or_insert(false);
			}
		}
		if let Method::Deployer = step.method {
			instances_we_must_deploy.swap_remove(&step.instance);
			instances_we_must_deploy.insert(step.instance.clone(), true);
		}

		let mut receipts = HashMap::new();
		for (instance, deploy_with_constructor_arguments) in instances_we_must_deploy.into_iter() {
			let calldata = deploy_with_constructor_arguments.then_some(&step.calldata);
			let value = deploy_with_constructor_arguments.then_some(step.value).flatten();

			let caller = {
				let context = self.default_resolution_context();
				let resolver = self.platform_information.node.resolver().await?;
				step.caller.resolve_address(resolver.as_ref(), context).await?
			};
			if let (_, _, Some(receipt)) = self
				.get_or_deploy_contract_instance(&instance, caller, calldata, value)
				.await
				.context("Failed to get or deploy contract instance during input execution")?
			{
				receipts.insert(instance.clone(), receipt);
			}
		}

		Ok(receipts)
	}

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_execution(
		&mut self,
		step: &FunctionCallStep,
		mut deployment_receipts: HashMap<ContractInstance, TransactionReceipt>,
	) -> Result<TransactionReceipt> {
		match step.method {
			// This step was already executed when `handle_step` was called. We just need to
			// lookup the transaction receipt in this case and continue on.
			Method::Deployer => deployment_receipts
				.remove(&step.instance)
				.context("Failed to find deployment receipt for constructor call"),
			Method::Fallback | Method::FunctionName(_) => {
				let resolver = self.platform_information.node.resolver().await?;
				let tx = match step
					.as_transaction(resolver.as_ref(), self.default_resolution_context())
					.await
				{
					Ok(tx) => tx,
					Err(err) => {
						return Err(err);
					},
				};

				self.platform_information.node.execute_transaction(tx).await
			},
		}
	}

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_call_frame_tracing(
		&mut self,
		tx_hash: TxHash,
	) -> Result<CallFrame> {
		self.platform_information
			.node
			.trace_transaction(
				tx_hash,
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

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_variable_assignment(
		&mut self,
		step: &FunctionCallStep,
		tracing_result: &CallFrame,
	) -> Result<()> {
		let Some(ref assignments) = step.variable_assignments else {
			return Ok(());
		};

		// Handling the return data variable assignments.
		for (variable_name, output_word) in assignments
			.return_data
			.iter()
			.zip(tracing_result.output.as_ref().unwrap_or_default().to_vec().chunks(32))
		{
			let value = U256::from_be_slice(output_word);
			self.execution_state.variables.insert(variable_name.clone(), value);
			tracing::info!(
				variable_name,
				variable_value = hex::encode(value.to_be_bytes::<32>()),
				"Assigned variable"
			);
		}

		Ok(())
	}

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_assertions(
		&mut self,
		step: &FunctionCallStep,
		receipt: &TransactionReceipt,
		tracing_result: &CallFrame,
	) -> Result<()> {
		// Resolving the `step.expected` into a series of expectations that we can then assert on.
		let mut expectations = match step {
			FunctionCallStep { expected: Some(Expected::Calldata(calldata)), .. } => {
				vec![ExpectedOutput::new().with_calldata(calldata.clone())]
			},
			FunctionCallStep { expected: Some(Expected::Expected(expected)), .. } => {
				vec![expected.clone()]
			},
			FunctionCallStep { expected: Some(Expected::ExpectedMany(expected)), .. } =>
				expected.clone(),
			FunctionCallStep { expected: None, .. } => vec![ExpectedOutput::new().with_success()],
		};

		// This is a bit of a special case and we have to support it separately on it's own. If it's
		// a call to the deployer method, then the tests will assert that it "returns" the address
		// of the contract. Deployments do not return the address of the contract but the runtime
		// code of the contracts. Therefore, this assertion would always fail. So, we replace it
		// with an assertion of "check if it succeeded"
		if let Method::Deployer = &step.method {
			for expectation in expectations.iter_mut() {
				expectation.return_data = None;
			}
		}

		futures::stream::iter(expectations.into_iter().map(Ok))
			.try_for_each_concurrent(None, |expectation| async {
				self.handle_function_call_assertion_item(receipt, tracing_result, expectation)
					.await
			})
			.await
	}

	#[instrument(level = "debug", skip_all)]
	async fn handle_function_call_assertion_item(
		&self,
		receipt: &TransactionReceipt,
		tracing_result: &CallFrame,
		assertion: ExpectedOutput,
	) -> Result<()> {
		let resolver = self
			.platform_information
			.node
			.resolver()
			.await
			.context("Failed to create the resolver for the node")?;

		if let Some(ref version_requirement) = assertion.compiler_version {
			if !version_requirement.matches(self.platform_information.compiler.version()) {
				return Ok(());
			}
		}

		let resolution_context = self
			.default_resolution_context()
			.with_block_number(receipt.block_number.as_ref())
			.with_transaction_hash(&receipt.transaction_hash);

		// Handling the receipt state assertion.
		let expected = !assertion.exception;
		let actual = receipt.status();
		if actual != expected {
			tracing::error!(
				expected,
				actual,
				?receipt,
				?tracing_result,
				"Transaction status assertion failed"
			);
			anyhow::bail!(
				"Transaction status assertion failed - Expected {expected} but got {actual}",
			);
		}

		// Handling the calldata assertion
		if let Some(ref expected_calldata) = assertion.return_data {
			let expected = expected_calldata;
			let actual = &tracing_result.output.as_ref().unwrap_or_default();
			if !expected
				.is_equivalent(actual, resolver.as_ref(), resolution_context)
				.await
				.context("Failed to resolve calldata equivalence for return data assertion")?
			{
				tracing::error!(
					?receipt,
					?expected,
					%actual,
					"Calldata assertion failed"
				);
				anyhow::bail!("Calldata assertion failed - Expected {expected:?} but got {actual}",);
			}
		}

		// Handling the events assertion
		if let Some(ref expected_events) = assertion.events {
			// Handling the events length assertion.
			let expected = expected_events.len();
			let actual = receipt.logs().len();
			if actual != expected {
				tracing::error!(expected, actual, "Event count assertion failed",);
				anyhow::bail!(
					"Event count assertion failed - Expected {expected} but got {actual}",
				);
			}

			// Handling the events assertion.
			for (event_idx, (expected_event, actual_event)) in
				expected_events.iter().zip(receipt.logs()).enumerate()
			{
				// Handling the emitter assertion.
				if let Some(ref expected_address) = expected_event.address {
					let expected = expected_address
						.resolve_address(resolver.as_ref(), resolution_context)
						.await?;
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
				for (expected, actual) in
					expected_event.topics.as_slice().iter().zip(actual_event.topics())
				{
					let expected = Calldata::new_compound([expected]);
					if !expected
						.is_equivalent(&actual.0, resolver.as_ref(), resolution_context)
						.await
						.context("Failed to resolve event topic equivalence")?
					{
						tracing::error!(
							event_idx,
							?receipt,
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
					.is_equivalent(&actual.0, resolver.as_ref(), resolution_context)
					.await
					.context("Failed to resolve event value equivalence")?
				{
					tracing::error!(
						event_idx,
						?receipt,
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

	#[instrument(level = "info", skip_all)]
	pub async fn execute_balance_assertion(
		&mut self,
		_: &StepPath,
		step: &BalanceAssertionStep,
	) -> anyhow::Result<usize> {
		self.step_address_auto_deployment(&step.address)
			.await
			.context("Failed to perform auto-deployment for the step address")?;

		let resolver = self.platform_information.node.resolver().await?;
		let address = step
			.address
			.resolve_address(resolver.as_ref(), self.default_resolution_context())
			.await?;

		let balance = self.platform_information.node.balance_of(address).await?;

		let expected = step.expected_balance;
		let actual = balance;
		if expected != actual {
			tracing::error!(%expected, %actual, %address, "Balance assertion failed");
			anyhow::bail!(
				"Balance assertion failed - Expected {} but got {} for {} resolved to {}",
				expected,
				actual,
				address,
				address,
			)
		}

		Ok(1)
	}

	#[instrument(level = "info", skip_all, err(Debug))]
	async fn execute_storage_empty_assertion_step(
		&mut self,
		_: &StepPath,
		step: &StorageEmptyAssertionStep,
	) -> Result<usize> {
		self.step_address_auto_deployment(&step.address)
			.await
			.context("Failed to perform auto-deployment for the step address")?;

		let resolver = self.platform_information.node.resolver().await?;
		let address = step
			.address
			.resolve_address(resolver.as_ref(), self.default_resolution_context())
			.await?;

		let storage = self
			.platform_information
			.node
			.latest_state_proof(address, Default::default())
			.await?;
		let is_empty = storage.storage_hash == EMPTY_ROOT_HASH;

		let expected = step.is_storage_empty;
		let actual = is_empty;

		if expected != actual {
			tracing::error!(%expected, %actual, %address, "Storage Empty Assertion failed");
			anyhow::bail!(
				"Storage Empty Assertion failed - Expected {} but got {} for {} resolved to {}",
				expected,
				actual,
				address,
				address,
			)
		};

		Ok(1)
	}

	#[instrument(level = "info", skip_all, err(Debug))]
	async fn execute_repeat_step(
		&mut self,
		step_path: &StepPath,
		step: &RepeatStep,
	) -> Result<usize> {
		let tasks = (0..step.repeat)
			.map(|_| PlatformDriver {
				platform_information: self.platform_information,
				test_definition: self.test_definition,
				private_key_allocator: self.private_key_allocator.clone(),
				execution_state: self.execution_state.clone(),
				steps_executed: 0,
				steps_iterator: {
					let steps: Vec<(StepPath, Step)> = step
						.steps
						.iter()
						.cloned()
						.enumerate()
						.map(|(step_idx, step)| {
							let step_idx = StepIdx::new(step_idx);
							let step_path = step_path.append(step_idx);
							(step_path, step)
						})
						.collect();
					steps.into_iter()
				},
			})
			.map(|driver| driver.execute_all())
			.collect::<Vec<_>>();
		let res = futures::future::try_join_all(tasks)
			.await
			.context("Repetition execution failed")?;
		Ok(res.first().copied().unwrap_or_default())
	}

	#[instrument(level = "info", skip_all, err(Debug))]
	pub async fn execute_account_allocation(
		&mut self,
		_: &StepPath,
		step: &AllocateAccountStep,
	) -> Result<usize> {
		let Some(variable_name) = step.variable_name.strip_prefix("$VARIABLE:") else {
			bail!("Account allocation must start with $VARIABLE:");
		};

		let private_key = self.private_key_allocator.lock().await.allocate()?;
		let account = private_key.address();
		let variable = U256::from_be_slice(account.0.as_slice());

		self.execution_state.variables.insert(variable_name.to_string(), variable);

		Ok(1)
	}
	// endregion:Step Handling

	// region:Contract Deployment
	#[instrument(
        level = "info",
        skip_all,
        fields(
            %contract_instance,
            %deployer
        ),
        err(Debug),
    )]
	async fn get_or_deploy_contract_instance(
		&mut self,
		contract_instance: &ContractInstance,
		deployer: Address,
		calldata: Option<&Calldata>,
		value: Option<EtherValue>,
	) -> Result<(Address, JsonAbi, Option<TransactionReceipt>)> {
		if let Some((_, address, abi)) =
			self.execution_state.deployed_contracts.get(contract_instance)
		{
			info!(

				%address,
				"Contract instance already deployed."
			);
			Ok((*address, abi.clone(), None))
		} else {
			info!("Contract instance requires deployment.");
			let (address, abi, receipt) = self
				.deploy_contract(contract_instance, deployer, calldata, value)
				.await
				.context("Failed to deploy contract")?;
			info!(
				%address,
				"Contract instance has been deployed."
			);
			Ok((address, abi, Some(receipt)))
		}
	}

	#[instrument(
    level = "info",
    skip_all,
        fields(
            %contract_instance,
            %deployer
        ),
        err(Debug),
    )]
	async fn deploy_contract(
		&mut self,
		contract_instance: &ContractInstance,
		deployer: Address,
		calldata: Option<&Calldata>,
		value: Option<EtherValue>,
	) -> Result<(Address, JsonAbi, TransactionReceipt)> {
		let Some(ContractPathAndIdent { contract_source_path, contract_ident }) =
			self.test_definition.metadata.contract_sources()?.remove(contract_instance)
		else {
			anyhow::bail!("Contract source not found for instance {:?}", contract_instance)
		};

		let Some((code, abi)) = self
			.execution_state
			.compiled_contracts
			.get(&contract_source_path)
			.and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
			.cloned()
		else {
			anyhow::bail!("Failed to find information for contract {:?}", contract_instance)
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
			},
		};

		if let Some(calldata) = calldata {
			let resolver = self.platform_information.node.resolver().await?;
			let calldata =
				calldata.calldata(resolver.as_ref(), self.default_resolution_context()).await?;
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

		let receipt = match self.platform_information.node.execute_transaction(tx).await {
			Ok(receipt) => receipt,
			Err(error) => {
				tracing::error!(?error, "Contract deployment transaction failed.");
				return Err(error);
			},
		};

		let Some(address) = receipt.contract_address else {
			anyhow::bail!("Contract deployment didn't return an address");
		};
		tracing::info!(
			instance_name = ?contract_instance,
			instance_address = ?address,
			"Deployed contract"
		);
		self.platform_information
			.reporter
			.report_contract_deployed_event(contract_instance.clone(), address)?;

		self.execution_state
			.deployed_contracts
			.insert(contract_instance.clone(), (contract_ident, address, abi.clone()));

		Ok((address, abi, receipt))
	}

	#[instrument(level = "info", skip_all)]
	async fn step_address_auto_deployment(
		&mut self,
		step_address: &StepAddress,
	) -> Result<Address> {
		match step_address {
			StepAddress::Address(address) => Ok(*address),
			StepAddress::ResolvableAddress(resolvable) => {
				let Some(instance) = resolvable.strip_suffix(".address").map(ContractInstance::new)
				else {
					bail!("Not an address variable");
				};

				self.get_or_deploy_contract_instance(
					&instance,
					FunctionCallStep::default_caller_address(),
					None,
					None,
				)
				.await
				.map(|v| v.0)
			},
		}
	}
	// endregion:Contract Deployment

	// region:Resolution & Resolver
	fn default_resolution_context(&self) -> ResolutionContext<'_> {
		ResolutionContext::default()
			.with_deployed_contracts(&self.execution_state.deployed_contracts)
			.with_variables(&self.execution_state.variables)
	}
	// endregion:Resolution & Resolver
}
