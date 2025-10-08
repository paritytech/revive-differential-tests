use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{
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
use indexmap::IndexMap;
use revive_dt_common::{
    futures::{PollingWaitBehavior, poll},
    types::PrivateKeyAllocator,
};
use revive_dt_format::{
    metadata::{ContractInstance, ContractPathAndIdent},
    steps::{
        AllocateAccountStep, BalanceAssertionStep, Calldata, EtherValue, FunctionCallStep, Method,
        RepeatStep, Step, StepAddress, StepIdx, StepPath, StorageEmptyAssertionStep,
    },
    traits::{ResolutionContext, ResolverApi},
};
use tokio::sync::{Mutex, mpsc::UnboundedSender};
use tracing::{Instrument, Span, debug, error, field::display, info, info_span, instrument};

use crate::{
    differential_benchmarks::{ExecutionState, WatcherEvent},
    helpers::{CachedCompiler, TestDefinition, TestPlatformInformation},
};

static DRIVER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The differential tests driver for a single platform.
pub struct Driver<'a, I> {
    /// The id of the driver.
    driver_id: usize,

    /// The information of the platform that this driver is for.
    platform_information: &'a TestPlatformInformation<'a>,

    /// The resolver of the platform.
    resolver: Arc<dyn ResolverApi + 'a>,

    /// The definition of the test that the driver is instructed to execute.
    test_definition: &'a TestDefinition<'a>,

    /// The private key allocator used by this driver and other drivers when account allocations
    /// are needed.
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,

    /// The execution state associated with the platform.
    execution_state: ExecutionState,

    /// The send side of the watcher's unbounded channel associated with this driver.
    watcher_tx: UnboundedSender<WatcherEvent>,

    /// The number of steps that were executed on the driver.
    steps_executed: usize,

    /// This is the queue of steps that are to be executed by the driver for this test case. Each
    /// time `execute_step` is called one of the steps is executed.
    steps_iterator: I,
}

impl<'a, I> Driver<'a, I>
where
    I: Iterator<Item = (StepPath, Step)>,
{
    // region:Constructors & Initialization
    pub async fn new(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        cached_compiler: &CachedCompiler<'a>,
        watcher_tx: UnboundedSender<WatcherEvent>,
        steps: I,
    ) -> Result<Self> {
        let mut this = Driver {
            driver_id: DRIVER_COUNT.fetch_add(1, Ordering::SeqCst),
            platform_information,
            resolver: platform_information
                .node
                .resolver()
                .await
                .context("Failed to create resolver")?,
            test_definition,
            private_key_allocator,
            execution_state: ExecutionState::empty(),
            steps_executed: 0,
            steps_iterator: steps,
            watcher_tx,
        };
        this.init_execution_state(cached_compiler)
            .await
            .context("Failed to initialize the execution state of the platform")?;
        Ok(this)
    }

    async fn init_execution_state(&mut self, cached_compiler: &CachedCompiler<'a>) -> Result<()> {
        let compiler_output = cached_compiler
            .compile_contracts(
                self.test_definition.metadata,
                self.test_definition.metadata_file_path,
                self.test_definition.mode.clone(),
                None,
                self.platform_information.compiler.as_ref(),
                self.platform_information.platform,
                &self.platform_information.reporter,
            )
            .await
            .inspect_err(|err| {
                error!(
                    ?err,
                    platform_identifier = %self.platform_information.platform.platform_identifier(),
                    "Pre-linking compilation failed"
                )
            })
            .context("Failed to produce the pre-linking compiled contracts")?;

        let mut deployed_libraries = None::<HashMap<_, _>>;
        let mut contract_sources = self
            .test_definition
            .metadata
            .contract_sources()
            .inspect_err(|err| {
                error!(
                    ?err,
                    platform_identifier = %self.platform_information.platform.platform_identifier(),
                    "Failed to retrieve contract sources from metadata"
                )
            })
            .context("Failed to get the contract instances from the metadata file")?;
        for library_instance in self
            .test_definition
            .metadata
            .libraries
            .iter()
            .flatten()
            .flat_map(|(_, map)| map.values())
        {
            debug!(%library_instance, "Deploying Library Instance");

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
            let deployer_address = self
                .test_definition
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
            let receipt = self.execute_transaction(tx).await.inspect_err(|err| {
                error!(
                    ?err,
                    %library_instance,
                    platform_identifier = %self.platform_information.platform.platform_identifier(),
                    "Failed to deploy the library"
                )
            })?;

            debug!(
                ?library_instance,
                platform_identifier = %self.platform_information.platform.platform_identifier(),
                "Deployed library"
            );

            let library_address = receipt
                .contract_address
                .expect("Failed to deploy the library");

            deployed_libraries.get_or_insert_default().insert(
                library_instance.clone(),
                (library_ident.clone(), library_address, abi.clone()),
            );
        }

        let compiler_output = cached_compiler
            .compile_contracts(
                self.test_definition.metadata,
                self.test_definition.metadata_file_path,
                self.test_definition.mode.clone(),
                deployed_libraries.as_ref(),
                self.platform_information.compiler.as_ref(),
                self.platform_information.platform,
                &self.platform_information.reporter,
            )
            .await
            .inspect_err(|err| {
                error!(
                    ?err,
                    platform_identifier = %self.platform_information.platform.platform_identifier(),
                    "Post-linking compilation failed"
                )
            })
            .context("Failed to compile the post-link contracts")?;

        self.execution_state = ExecutionState::new(
            compiler_output.contracts,
            deployed_libraries.unwrap_or_default(),
        );

        Ok(())
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
            driver_id = self.driver_id,
            platform_identifier = %self.platform_information.platform.platform_identifier(),
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
            Step::Repeat(step) => self
                .execute_repeat_step(step_path, step.as_ref())
                .await
                .context("Repetition Step Failed"),
            Step::AllocateAccount(step) => self
                .execute_account_allocation(step_path, step.as_ref())
                .await
                .context("Account Allocation Step Failed"),
            // The following steps are disabled in the benchmarking driver.
            Step::BalanceAssertion(..) | Step::StorageEmptyAssertion(..) => Ok(0),
        }?;
        self.steps_executed += steps_executed;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id))]
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
        Ok(1)
    }

    async fn handle_function_call_contract_deployment(
        &mut self,
        step: &FunctionCallStep,
    ) -> Result<HashMap<ContractInstance, TransactionReceipt>> {
        let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
        for instance in step.find_all_contract_instances().into_iter() {
            if !self
                .execution_state
                .deployed_contracts
                .contains_key(&instance)
            {
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
            let value = deploy_with_constructor_arguments
                .then_some(step.value)
                .flatten();

            let caller = {
                let context = self.default_resolution_context();
                step.caller
                    .resolve_address(self.resolver.as_ref(), context)
                    .await?
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
                let tx = step
                    .as_transaction(self.resolver.as_ref(), self.default_resolution_context())
                    .await?;
                self.execute_transaction(tx).await
            }
        }
    }

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

    async fn handle_function_call_variable_assignment(
        &mut self,
        step: &FunctionCallStep,
        tracing_result: &CallFrame,
    ) -> Result<()> {
        let Some(ref assignments) = step.variable_assignments else {
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
            self.execution_state
                .variables
                .insert(variable_name.clone(), value);
            tracing::info!(
                variable_name,
                variable_value = hex::encode(value.to_be_bytes::<32>()),
                "Assigned variable"
            );
        }

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id))]
    pub async fn execute_balance_assertion(
        &mut self,
        _: &StepPath,
        _: &BalanceAssertionStep,
    ) -> anyhow::Result<usize> {
        // Kept empty intentionally for the benchmark driver.
        Ok(1)
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id), err(Debug))]
    async fn execute_storage_empty_assertion_step(
        &mut self,
        _: &StepPath,
        _: &StorageEmptyAssertionStep,
    ) -> Result<usize> {
        // Kept empty intentionally for the benchmark driver.
        Ok(1)
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id), err(Debug))]
    async fn execute_repeat_step(
        &mut self,
        step_path: &StepPath,
        step: &RepeatStep,
    ) -> Result<usize> {
        let tasks = (0..step.repeat)
            .map(|_| Driver {
                driver_id: DRIVER_COUNT.fetch_add(1, Ordering::SeqCst),
                platform_information: self.platform_information,
                resolver: self.resolver.clone(),
                test_definition: self.test_definition,
                private_key_allocator: self.private_key_allocator.clone(),
                execution_state: self.execution_state.clone(),
                steps_executed: 0,
                steps_iterator: {
                    let steps = step
                        .steps
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(step_idx, step)| {
                            let step_idx = StepIdx::new(step_idx);
                            let step_path = step_path.append(step_idx);
                            (step_path, step)
                        })
                        .collect::<Vec<_>>();
                    steps.into_iter()
                },
                watcher_tx: self.watcher_tx.clone(),
            })
            .map(|driver| driver.execute_all());

        // TODO: Determine how we want to know the `ignore_block_before` and if it's through the
        // receipt and how this would impact the architecture and the possibility of us not waiting
        // for receipts in the future.
        self.watcher_tx
            .send(WatcherEvent::RepetitionStartEvent {
                ignore_block_before: 0,
            })
            .context("Failed to send message on the watcher's tx")?;

        let res = futures::future::try_join_all(tasks)
            .await
            .context("Repetition execution failed")?;
        Ok(res.into_iter().sum())
    }

    #[instrument(level = "info", fields(driver_id = self.driver_id), skip_all, err(Debug))]
    pub async fn execute_account_allocation(
        &mut self,
        _: &StepPath,
        step: &AllocateAccountStep,
    ) -> Result<usize> {
        let Some(variable_name) = step.variable_name.strip_prefix("$VARIABLE:") else {
            bail!("Account allocation must start with $VARIABLE:");
        };

        let private_key = self
            .private_key_allocator
            .lock()
            .await
            .allocate()
            .context("Account allocation through the private key allocator failed")?;
        let account = private_key.address();
        let variable = U256::from_be_slice(account.0.as_slice());

        self.execution_state
            .variables
            .insert(variable_name.to_string(), variable);

        Ok(1)
    }
    // endregion:Step Handling

    // region:Contract Deployment
    #[instrument(
        level = "info",
        skip_all,
        fields(
            driver_id = self.driver_id,
            platform_identifier = %self.platform_information.platform.platform_identifier(),
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
        if let Some((_, address, abi)) = self
            .execution_state
            .deployed_contracts
            .get(contract_instance)
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
            driver_id = self.driver_id,
            platform_identifier = %self.platform_information.platform.platform_identifier(),
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
        let Some(ContractPathAndIdent {
            contract_source_path,
            contract_ident,
        }) = self
            .test_definition
            .metadata
            .contract_sources()?
            .remove(contract_instance)
        else {
            anyhow::bail!(
                "Contract source not found for instance {:?}",
                contract_instance
            )
        };

        let Some((code, abi)) = self
            .execution_state
            .compiled_contracts
            .get(&contract_source_path)
            .and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
            .cloned()
        else {
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
                .calldata(self.resolver.as_ref(), self.default_resolution_context())
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

        let receipt = match self.execute_transaction(tx).await {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::error!(?error, "Contract deployment transaction failed.");
                return Err(error);
            }
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

        self.execution_state.deployed_contracts.insert(
            contract_instance.clone(),
            (contract_ident, address, abi.clone()),
        );

        Ok((address, abi, receipt))
    }

    #[instrument(level = "info", fields(driver_id = self.driver_id), skip_all)]
    async fn step_address_auto_deployment(
        &mut self,
        step_address: &StepAddress,
    ) -> Result<Address> {
        match step_address {
            StepAddress::Address(address) => Ok(*address),
            StepAddress::ResolvableAddress(resolvable) => {
                let Some(instance) = resolvable
                    .strip_suffix(".address")
                    .map(ContractInstance::new)
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
            }
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

    // region:Transaction Execution
    /// Executes the transaction on the driver's node with some custom waiting logic for the receipt
    #[instrument(
        level = "info",
        skip_all,
        fields(driver_id = self.driver_id, transaction_hash = tracing::field::Empty)
    )]
    async fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt> {
        let node = self.platform_information.node;
        let transaction_hash = node
            .submit_transaction(transaction)
            .await
            .context("Failed to submit transaction")?;
        Span::current().record("transaction_hash", display(transaction_hash));

        info!("Submitted transaction");
        self.watcher_tx
            .send(WatcherEvent::SubmittedTransaction { transaction_hash })
            .context("Failed to send the transaction hash to the watcher")?;

        info!("Starting to poll for transaction receipt");
        poll(
            Duration::from_secs(30 * 60),
            PollingWaitBehavior::Constant(Duration::from_secs(1)),
            || {
                async move {
                    match node.get_receipt(transaction_hash).await {
                        Ok(receipt) => {
                            info!("Polling succeeded, receipt found");
                            Ok(ControlFlow::Break(receipt))
                        }
                        Err(_) => Ok(ControlFlow::Continue(())),
                    }
                }
                .instrument(info_span!("Polling for receipt"))
            },
        )
        .await
    }
    // endregion:Transaction Execution
}
