use pallet_revive::evm::Trace;
use revive_dt_node_interaction::revive_metadata::runtime_apis::revive_api::types::trace_tx;
use revive_dt_node_interaction::revive_metadata::runtime_types::pallet_revive::evm::api::debug_rpc_types::{TracerType, CallTracerConfig};
use subxt::utils::UncheckedExtrinsic;

use crate::internal_prelude::*;

use crate::differential_benchmarks::ExecutionState;

fn next_driver_id() -> usize {
    static DRIVER_COUNT: StdMutex<usize> = StdMutex::new(0);
    let mut guard = DRIVER_COUNT.lock().expect("poisoned");
    let count = *guard;
    *guard += 1;
    count
}

/// The differential tests driver for a single platform.
pub struct Driver<'a, I> {
    /// The id of the driver.
    driver_id: usize,

    /// The information of the platform that this driver is for.
    platform_information: &'a TestPlatformInformation<'a>,

    /// The definition of the test that the driver is instructed to execute.
    test_definition: &'a TestDefinition<'a>,

    /// A value override to apply to the repetition count of this run of the driver.
    repetition_count_override: Option<usize>,

    /// The private key allocator used by this driver and other drivers when account allocations are
    /// needed.
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,

    /// The execution state associated with the platform.
    execution_state: ExecutionState,

    /// The send side of the watcher's unbounded channel associated with this driver.
    watcher_tx: UnboundedSender<WatcherEvent>,

    /// The number of steps that were executed on the driver.
    steps_executed: usize,

    /// A watcher used to watch for the inclusion of transactions in a block, which is better than
    /// polling for their receipts and clogging up the network.
    inclusion_watcher: &'a InclusionWatcher,

    /// A map of the gas limit for all of the transactions we have.
    gas_limits: Arc<RwLock<HashMap<StepPath, u64>>>,

    /// This field controls if the driver should wait for transactions to be included in a block or
    /// not before proceeding forward.
    await_transaction_inclusion: bool,

    /// This field controls if the driver should wait for transaction receipts or not before
    /// proceeding forward.
    await_transaction_receipts: bool,

    /// A service which allows us to find the specific block which contains a specific transaction.
    transaction_finder: TransactionFinder,

    /// This is the queue of steps that are to be executed by the driver for this test case. Each
    /// time `execute_step` is called one of the steps is executed.
    steps_iterator: I,
}

impl<'a, I> Driver<'a, I>
where
    I: Iterator<Item = (StepPath, Step)>,
{
    // region:Constructors & Initialization
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        repetition_count_override: Option<usize>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        cached_compiler: &CachedCompiler<'a>,
        watcher_tx: UnboundedSender<WatcherEvent>,
        await_transaction_inclusion: bool,
        inclusion_watcher: &'a InclusionWatcher,
        transaction_finder: TransactionFinder,
        steps: I,
    ) -> Result<Self> {
        let mut this = Driver {
            driver_id: next_driver_id(),
            platform_information,
            test_definition,
            repetition_count_override,
            private_key_allocator,
            execution_state: ExecutionState::empty(),
            steps_executed: 0,
            steps_iterator: steps,
            inclusion_watcher,
            await_transaction_inclusion,
            await_transaction_receipts: true,
            watcher_tx,
            gas_limits: Arc::new(Default::default()),
            transaction_finder,
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
                self.platform_information.platform.compiler_identifier(),
                Some(self.platform_information.platform.platform_identifier()),
                &CompilationReporter::Execution(&self.platform_information.reporter),
            )
            .await
            .inspect_err(|err| error!(?err, "Pre-linking compilation failed"))
            .context("Failed to produce the pre-linking compiled contracts")?;

        let deployer_address = self.test_definition.case.deployer_address();

        let mut deployed_libraries = None::<HashMap<_, _>>;
        let mut contract_sources = self
            .test_definition
            .metadata
            .contract_sources()
            .inspect_err(|err| error!(?err, "Failed to retrieve contract sources from metadata"))
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

            let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                TransactionRequest::default().from(deployer_address),
                code,
            );
            let receipt = self
                .execute_transaction(tx, None, Duration::from_secs(5 * 60))
                .and_then(|(_, receipt_fut, _)| receipt_fut)
                .await
                .inspect_err(|err| {
                    error!(
                        ?err,
                        %library_instance,
                        "Failed to deploy the library"
                    )
                })?;

            debug!(?library_instance, "Deployed library");

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
                self.platform_information.platform.compiler_identifier(),
                Some(self.platform_information.platform.platform_identifier()),
                &CompilationReporter::Execution(&self.platform_information.reporter),
            )
            .await
            .inspect_err(|err| error!(?err, "Post-linking compilation failed"))
            .context("Failed to compile the post-link contracts")?;

        // Factory contracts on the PVM refer to the code that they're instantiating by hash rather
        // than including the actual bytecode. This creates a problem where a factory contract could
        // be deployed but the code it's supposed to create is not on chain. Therefore, we upload
        // all the code to the chain prior to running any transactions on the driver.
        if let Some(substrate_provider_fut) = self.platform_information.node.substrate_provider()
            && self.platform_information.platform.vm_identifier() == VmIdentifier::PolkaVM
        {
            let substrate_client = substrate_provider_fut
                .await
                .context("Failed to connect to the substrate node")?;
            let metadata = substrate_client.metadata();

            const RUNTIME_PALLET_ADDRESS: Address =
                address!("0x6d6f646c70792f70616464720000000000000000");

            let code_upload_tasks = compiler_output
                .contracts
                .values()
                .flat_map(|item| item.values())
                .map(|(code_string, _)| {
                    let metadata = metadata.clone();
                    let node = self.platform_information.node;
                    async move {
                        let code = alloy::hex::decode(code_string)
                            .context("Failed to hex-decode the post-link code. This is a bug")?;
                        let upload_call = subxt::dynamic::tx(
                            "Revive",
                            "upload_code",
                            vec![
                                subxt::dynamic::Value::from_bytes(code),
                                subxt::dynamic::Value::u128(u128::MAX),
                            ],
                        );
                        let encoded_payload = upload_call
                            .encode_call_data(&metadata)
                            .context("Failed to encode the upload code payload")?;

                        let tx_request = TransactionRequest::default()
                            .from(deployer_address)
                            .to(RUNTIME_PALLET_ADDRESS)
                            .input(encoded_payload.into());
                        node.execute_transaction(tx_request)
                            .await
                            .context("Failed to execute transaction")
                    }
                });
            try_join_all(code_upload_tasks)
                .await
                .context("Code upload failed")?;
        }

        for (contract_path, contract_name_to_info_mapping) in compiler_output.contracts.iter() {
            for (contract_name, (contract_bytecode, _)) in contract_name_to_info_mapping.iter() {
                let contract_bytecode = hex::decode(contract_bytecode)
                    .expect("Impossible for us to get an undecodable bytecode after linking");

                self.platform_information
                    .reporter
                    .report_contract_information_event(
                        contract_path.to_path_buf(),
                        contract_name.clone(),
                        contract_bytecode.len(),
                    )
                    .expect("Should not fail");
            }
        }

        self.execution_state = ExecutionState::new(
            compiler_output.contracts,
            deployed_libraries.unwrap_or_default(),
        );

        Ok(())
    }
    // endregion:Constructors & Initialization

    // region:Step Handling
    pub async fn execute_all(mut self) -> Result<(usize, ExecutionState)> {
        while let Some(result) = self.execute_next_step().await {
            result?
        }
        Ok((self.steps_executed, self.execution_state))
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
            %step_path,
            is_warm_up = self.await_transaction_receipts,
        ),
        err(Debug),
    )]
    async fn execute_step(&mut self, step_path: &StepPath, step: &Step) -> Result<()> {
        self.platform_information.node.provider().await.unwrap();
        let steps_executed = match step {
            Step::FunctionCall(step) => {
                // If a function call step fails then we stop this driver and allow the other
                // drivers to continue.
                match self
                    .execute_function_call(step_path, step.as_ref())
                    .await
                    .context("Function call step Failed")
                {
                    Ok(steps_executed) => Ok(steps_executed),
                    Err(err) => {
                        warn!(?err, "Step execution failed");
                        return Ok(());
                    }
                }
            }
            Step::Repeat(step) => self
                .execute_repeat_step(step_path, step.as_ref())
                .await
                .context("Repetition Step Failed"),
            Step::AllocateAccount(step) => self
                .execute_account_allocation(step_path, step.as_ref())
                .await
                .context("Account Allocation Step Failed"),
            Step::Transfer(step) => match self
                .execute_transfer(step_path, step.as_ref())
                .await
                .context("Transfer Step Failed")
            {
                Ok(steps_executed) => Ok(steps_executed),
                Err(err) => {
                    warn!(?err, "Step execution failed");
                    return Ok(());
                }
            },
            // The following steps are disabled in the benchmarking driver.
            Step::BalanceAssertion(..) | Step::StorageEmptyAssertion(..) => Ok(0),
        }?;
        self.steps_executed += steps_executed;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id))]
    pub async fn execute_function_call(
        &mut self,
        step_path: &StepPath,
        step: &FunctionCallStep,
    ) -> Result<usize> {
        let deployment_receipts = self
            .handle_function_call_contract_deployment(step_path, step)
            .await
            .context("Failed to deploy contracts for the function call step")?;
        let (transaction_hash, receipt) = self
            .handle_function_call_execution(step_path, step, deployment_receipts)
            .await
            .context("Failed to handle the function call execution")?;
        self.handle_function_call_variable_assignment(step, transaction_hash, receipt.as_ref())
            .await
            .context("Failed to handle function call variable assignment")?;
        Ok(1)
    }

    #[instrument(level = "info", skip_all, fields(driver_id = self.driver_id))]
    pub async fn execute_transfer(
        &mut self,
        step_path: &StepPath,
        step: &TransferStep,
    ) -> Result<usize> {
        let context = self.default_resolution_context();
        let from = step
            .from
            .resolve_address(self.platform_information.node, context)
            .await
            .context("Failed to resolve transfer sender")?;
        let to = step
            .to
            .resolve_address(self.platform_information.node, context)
            .await
            .context("Failed to resolve transfer recipient")?;
        let tx = TransactionRequest::default()
            .from(from)
            .to(to)
            .value(step.amount.into_inner());
        let (_tx_hash, receipt_future, inclusion_future) = self
            .execute_transaction(tx, Some(step_path), Duration::from_secs(30 * 60))
            .await
            .context("Failed to submit transfer transaction")?;
        if self.await_transaction_receipts {
            let receipt = receipt_future.await?;
            if !receipt.status() {
                bail!("Transfer transaction failed {receipt:?}");
            }
        } else if self.await_transaction_inclusion {
            inclusion_future.await?;
        };
        Ok(1)
    }

    async fn handle_function_call_contract_deployment(
        &mut self,
        step_path: &StepPath,
        step: &FunctionCallStep,
    ) -> Result<HashMap<ContractInstance, TransactionReceipt>> {
        let mut instances_we_must_deploy = IndexMap::<ContractInstance, bool>::new();
        for instance in step
            .find_all_contract_instances(self.default_resolution_context())
            .await
            .into_iter()
        {
            if !self
                .execution_state
                .deployed_contracts
                .contains_key(&instance)
            {
                instances_we_must_deploy.entry(instance).or_insert(false);
            }
        }
        if let Method::Deployer = step.method {
            let resolution_context = self.default_resolution_context();
            let instance = resolution_context
                .contract_instance(&step.instance)
                .await
                .context("Failed to get the step's instance")?;

            instances_we_must_deploy.swap_remove(&instance);
            instances_we_must_deploy.insert(instance, true);
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
                    .resolve_address(self.platform_information.node, context)
                    .await?
            };
            if let (_, _, Some(receipt)) = self
                .get_or_deploy_contract_instance(
                    &instance,
                    caller,
                    calldata,
                    value,
                    Some(step_path),
                )
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
        step_path: &StepPath,
        step: &FunctionCallStep,
        mut deployment_receipts: HashMap<ContractInstance, TransactionReceipt>,
    ) -> Result<(TxHash, Option<TransactionReceipt>)> {
        match step.method {
            // This step was already executed when `handle_step` was called. We just need to
            // lookup the transaction receipt in this case and continue on.
            Method::Deployer => {
                let instance = self
                    .default_resolution_context()
                    .contract_instance(&step.instance)
                    .await
                    .context("Failed to resolve the step's instance")?;
                let receipt = deployment_receipts
                    .remove(&instance)
                    .context("Failed to find deployment receipt for constructor call")?;
                let tx_hash = receipt.transaction_hash;
                Ok((tx_hash, Some(receipt)))
            }
            Method::Fallback | Method::FunctionName(_) => {
                let mut tx = step
                    .as_transaction(
                        self.platform_information.node,
                        self.default_resolution_context(),
                    )
                    .await?;

                let gas_overrides = step
                    .gas_overrides
                    .get(&self.platform_information.platform.platform_identifier())
                    .copied()
                    .unwrap_or_default();
                gas_overrides.apply_to::<Ethereum>(&mut tx);

                let (tx_hash, receipt_future, inclusion_future) = self
                    .execute_transaction(tx.clone(), Some(step_path), Duration::from_secs(30 * 60))
                    .await?;
                let receipt =
                    if self.await_transaction_receipts || step.variable_assignments.is_some() {
                        let receipt = receipt_future.await?;
                        if !receipt.status() {
                            bail!("Transaction failed {receipt:?}");
                        }
                        Some(receipt)
                    } else {
                        if self.await_transaction_inclusion {
                            inclusion_future.await?;
                        }
                        None
                    };

                Ok((tx_hash, receipt))
            }
        }
    }

    async fn handle_function_call_call_frame_tracing(
        &mut self,
        tx_hash: TxHash,
    ) -> Result<CallFrame> {
        let (transaction_index, transaction_block) = self.transaction_finder.find(tx_hash).await;

        match transaction_block.substrate_block {
            Some(ref substrate_block) => {
                let substrate_block_parent_hash = substrate_block.header().parent_hash;

                let header = Decode::decode(&mut substrate_block.header().encode().as_slice())
                    .context("Failed to decode the header into the runtime header type")?;
                let extrinsics = substrate_block
                    .extrinsics()
                    .await
                    .context("Failed to get the extrinsics of the substrate block")?
                    .iter()
                    .map(|extrinsic| UncheckedExtrinsic::new(extrinsic.bytes().to_vec()))
                    .collect();

                let trace_call = revive_metadata::apis()
                    .revive_api()
                    .trace_tx(
                        trace_tx::Block { header, extrinsics },
                        transaction_index as _,
                        TracerType::CallTracer(Some(CallTracerConfig {
                            with_logs: true,
                            only_top_call: false,
                        })),
                    )
                    .unvalidated();
                let trace = self
                    .platform_information
                    .node
                    .substrate_provider()
                    .expect("This is a substrate based node")
                    .await
                    .context("Failed to get the substrate provider for the node")?
                    .runtime_api()
                    .at(substrate_block_parent_hash)
                    .call(trace_call)
                    .await
                    .context("Failed to get the transaction trace")?
                    .context("Failed to get the transaction trace")?;

                let trace = Trace::decode(&mut trace.encode().as_slice())
                    .context("Failed to decode the trace into the pallet-revive trace type")?;
                let Trace::Call(call_trace) = trace else {
                    bail!("Requested a call trace but got a prestate trace back")
                };
                let serialized = serde_json::to_value(call_trace)
                    .context("Failed to serialize the pallet-revive call trace")?;
                serde_json::from_value::<CallFrame>(serialized)
                    .context("Failed to deserialize the call trace into the alloy type")
            }
            None => self
                .platform_information
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
                    trace.try_into_call_frame().expect(
                        "Impossible - we requested a callframe trace so we must get it back",
                    )
                }),
        }
    }

    async fn handle_function_call_variable_assignment(
        &mut self,
        step: &FunctionCallStep,
        tx_hash: TxHash,
        receipt: Option<&TransactionReceipt>,
    ) -> Result<()> {
        let Some(ref assignments) = step.variable_assignments else {
            return Ok(());
        };

        // Collect 32-byte value words for each variable to assign, based on the source.
        let value_words: Vec<[u8; 32]> = match assignments {
            VariableAssignments::EventTopics { topics, .. } => {
                let receipt = receipt.context(
                    "event_topics capture requires the receipt; \
                     ensure await_transaction_receipts is set or step.variable_assignments is present",
                )?;
                extract_event_topic_values(receipt.inner.logs(), topics)?
            }
            VariableAssignments::ReturnData { .. } => {
                let callframe = self
                    .handle_function_call_call_frame_tracing(tx_hash)
                    .await
                    .context("Failed to get the callframe trace for transaction")?;
                callframe
                    .output
                    .as_ref()
                    .unwrap_or_default()
                    .to_vec()
                    .chunks(32)
                    .map(|chunk| {
                        let mut padded = [0u8; 32];
                        padded[..chunk.len()].copy_from_slice(chunk);
                        padded
                    })
                    .collect()
            }
        };

        for (raw_name, value_word) in assignments.names().iter().zip(value_words.iter()) {
            let variable_name: String = if raw_name.contains("$VARIABLE:") {
                let context = self.default_resolution_context();
                revive_dt_format::steps::CalldataToken::<&str>::resolve_variable_name_template(
                    raw_name, context,
                )
                .context("Failed to resolve variable_assignments templated variable name")?
            } else {
                raw_name.clone()
            };
            let value = U256::from_be_slice(value_word);
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

    fn execute_repeat_step<'b>(
        &'b mut self,
        step_path: &'b StepPath,
        step: &'b RepeatStep,
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + 'b>> {
        let driver_id = self.driver_id;
        Box::pin(
            async move {
                let mut tasks = (0..self.repetition_count_override.unwrap_or(step.repeat))
                    .map(|i| {
                        let mut execution_state = self.execution_state.clone();
                        if let Some(ref index_variable) = step
                            .capture_index
                            .as_ref()
                            .and_then(|capture_index| capture_index.strip_prefix("$VARIABLE:"))
                        {
                            execution_state.variables.insert(
                                index_variable.to_string(),
                                i.try_into()
                                    .expect("Can't fail, we're going into a larger number of bits"),
                            );
                        }

                        Driver {
                            driver_id: next_driver_id(),
                            platform_information: self.platform_information,
                            test_definition: self.test_definition,
                            repetition_count_override: self.repetition_count_override,
                            private_key_allocator: self.private_key_allocator.clone(),
                            execution_state,
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
                            await_transaction_inclusion: step.await_transaction_inclusion,
                            await_transaction_receipts: self.await_transaction_receipts && i == 0,
                            inclusion_watcher: self.inclusion_watcher,
                            watcher_tx: self.watcher_tx.clone(),
                            gas_limits: self.gas_limits.clone(),
                            transaction_finder: self.transaction_finder.clone(),
                        }
                    })
                    .map(|driver| driver.execute_all());

                // We run just one of the drivers first before all of the other drivers to allow it to cache
                // the gas limits for all of the subsequent runs.
                let Some(first_task) = tasks.next() else {
                    return Ok(0);
                };
                let (first_res, first_execution_state) = Box::pin(first_task)
                    .await
                    .context("Running the first initialization driver failed")?;

                // Send the start event to the watcher after the first (warm-up) driver has
                // completed only if this repeat step is marked as the benchmark start point.
                // This ensures that blocks produced during setup steps (e.g., contract
                // deployments) are excluded from the benchmark metrics.
                if step.start_watcher {
                    self.watcher_tx
                        .send(WatcherEvent::StartEvent {
                            ignore_block_before: self
                                .platform_information
                                .node
                                .provider()
                                .await
                                .context("Failed to get the provider")?
                                .get_block_number()
                                .await
                                .context("Failed to get the block number of the latest block")?,
                        })
                        .context("Failed to send message on the watcher's tx")?;
                }

                let (steps_executed, execution_states) = futures::future::try_join_all(tasks)
                    .await
                    .context("Repetition execution failed")?
                    .into_iter()
                    .unzip::<_, _, Vec<_>, Vec<_>>();

                if step.consolidate_state {
                    self.execution_state.add_state(first_execution_state);
                    for state in execution_states.into_iter() {
                        self.execution_state.add_state(state);
                    }
                }

                Ok(steps_executed.into_iter().sum::<usize>() + first_res)
            }
            .instrument(info_span!("execute_repeat_step", driver_id,)),
        )
    }

    #[instrument(level = "info", fields(driver_id = self.driver_id), skip_all, err(Debug))]
    pub async fn execute_account_allocation(
        &mut self,
        _: &StepPath,
        step: &AllocateAccountStep,
    ) -> Result<usize> {
        let Some(raw_name) = step.variable_name.strip_prefix("$VARIABLE:") else {
            bail!("Account allocation must start with $VARIABLE:");
        };

        let variable_name: String = if raw_name.contains("$VARIABLE:") {
            let context = self.default_resolution_context();
            revive_dt_format::steps::CalldataToken::<&str>::resolve_variable_name_template(
                raw_name, context,
            )
            .context("Failed to resolve allocate_account templated name")?
        } else {
            raw_name.to_owned()
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
            .insert(variable_name, variable);

        Ok(1)
    }
    // endregion:Step Handling

    // region:Contract Deployment
    #[instrument(
        level = "info",
        skip_all,
        fields(
            driver_id = self.driver_id,
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
        step_path: Option<&StepPath>,
    ) -> Result<(Address, JsonAbi, Option<TransactionReceipt>)> {
        if let Some(entry) = self
            .execution_state
            .deployed_contracts
            .get(contract_instance)
        {
            let (_, address, abi) = entry.as_ref();
            info!(

                %address,
                "Contract instance already deployed."
            );
            Ok((*address, abi.clone(), None))
        } else {
            info!("Contract instance requires deployment.");
            let (address, abi, receipt) = self
                .deploy_contract(contract_instance, deployer, calldata, value, step_path)
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
        step_path: Option<&StepPath>,
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

        let compiled_contracts = self.execution_state.compiled_contracts.lock().await;
        let Some((code, abi)) = compiled_contracts
            .get(&contract_source_path)
            .and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
            .cloned()
        else {
            anyhow::bail!(
                "Failed to find information for contract {:?}",
                contract_instance
            )
        };
        drop(compiled_contracts);

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
                .calldata(
                    self.platform_information.node,
                    self.default_resolution_context(),
                )
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

        let receipt = match self
            .execute_transaction(tx, step_path, Duration::from_secs(5 * 60))
            .and_then(|(_, receipt_fut, _)| receipt_fut)
            .await
        {
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
            Arc::new((contract_ident, address, abi.clone())),
        );

        Ok((address, abi, receipt))
    }
    // endregion:Contract Deployment

    // region:Resolution & Resolver
    fn default_resolution_context(&self) -> ResolutionContext<'_> {
        ResolutionContext::default()
            .with_deployed_contracts(&self.execution_state.deployed_contracts)
            .with_variables(&self.execution_state.variables)
            .with_metadata(&self.test_definition.metadata.content)
            .with_node_api(self.platform_information.node)
    }
    // endregion:Resolution & Resolver

    // region:Transaction Execution
    /// Executes the transaction on the driver's node with some custom waiting logic for the receipt
    #[instrument(
        level = "info",
        skip_all,
        fields(
            driver_id = self.driver_id,
            transaction_hash = tracing::field::Empty
        ),
        err(Debug)
    )]
    async fn execute_transaction(
        &self,
        mut transaction: TransactionRequest,
        step_path: Option<&StepPath>,
        receipt_wait_duration: Duration,
    ) -> anyhow::Result<(
        TxHash,
        impl Future<Output = Result<TransactionReceipt>> + Send + 'static,
        impl Future<Output = Result<()>> + Send + 'static,
    )> {
        let node = self.platform_information.node;
        let provider = node.provider().await.context("Creating provider failed")?;

        if let Some(step_path) = step_path
            && self.platform_information.platform.allow_caching_gas_limit()
        {
            let read_guard = self.gas_limits.read().await;
            let gas_limit = match read_guard.get(step_path) {
                Some(gas_estimate) => {
                    info!(
                        estimated_gas = gas_estimate,
                        step_path = %step_path,
                        "Obtained gas estimate from cache"
                    );
                    *gas_estimate
                }
                None => {
                    drop(read_guard);
                    let mut write_guard = self.gas_limits.write().await;
                    match write_guard.get(step_path) {
                        Some(gas_estimate) => {
                            warn!("False positive in gas estimation cache");
                            *gas_estimate
                        }
                        None => {
                            let gas_estimate = timeout(Duration::from_secs(5 * 60), async {
                                let mut interval = interval(Duration::from_millis(200));
                                loop {
                                    interval.tick().await;
                                    match provider.estimate_gas(transaction.clone()).await {
                                        Ok(gas_estimate) => break gas_estimate,
                                        Err(err) => {
                                            warn!(?err, "Failed to get the gas estimate, retrying");
                                            continue;
                                        }
                                    }
                                }
                            })
                            .await
                            .context("Failed to get the gas estimate")?;
                            write_guard.insert(step_path.clone(), gas_estimate);
                            info!(
                                estimated_gas = gas_estimate,
                                step_path = %step_path,
                                "Initialized gas estimate into cache"
                            );
                            gas_estimate
                        }
                    }
                }
            };
            transaction.set_gas_limit(gas_limit * 120 / 100);
        }

        let transaction_hash = self
            .platform_information
            .node
            .submit_transaction(transaction)
            .await
            .context("Failed to submit transaction")?;
        let pending_transaction_builder =
            PendingTransactionBuilder::new(provider.root().clone(), transaction_hash);
        Span::current().record("transaction_hash", display(transaction_hash));

        info!(%transaction_hash, "Submitted transaction");
        if let Some(step_path) = step_path {
            self.watcher_tx
                .send(WatcherEvent::SubmittedTransaction {
                    transaction_hash,
                    step_path: step_path.clone(),
                    submission_time: SystemTime::now(),
                })
                .context("Failed to send the transaction hash to the watcher")?;
        };

        let inclusion_provider = provider.clone();
        let receipt_future = async move {
            let receipt = pending_transaction_builder
                .with_timeout(Some(receipt_wait_duration))
                .with_required_confirmations(2)
                .get_receipt()
                .inspect_ok(|receipt| info!(transaction_hash = %receipt.transaction_hash, "Obtained receipt"))
                .map(|res| res.context("Failed to get the receipt of the transaction")).await?;
            let block_number = receipt
                .block_number
                .context("Receipt must have a block number")?;
            tokio::time::timeout(Duration::from_secs(120), async move {
                while provider
                    .get_block_number()
                    .await
                    .context("Failed to get the block number")?
                    < block_number
                {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Result::<(), anyhow::Error>::Ok(())
            })
            .await
            .context("Waited 120 seconds for the rpc block to be updated but it wasn't")??;
            anyhow::Result::<_, anyhow::Error>::Ok(receipt)
        };

        let inclusion_future = {
            let await_inclusion = self.inclusion_watcher.await_transaction(transaction_hash);
            async move {
                let block_number = await_inclusion.await;
                tokio::time::timeout(Duration::from_secs(120), async move {
                    while inclusion_provider
                        .get_block_number()
                        .await
                        .context("Failed to get the block number")?
                        < block_number
                    {
                        tokio::time::sleep(Duration::from_millis(200)).await
                    }
                    Result::<(), anyhow::Error>::Ok(())
                })
                .await
                .context("Waited 120 seconds for the rpc block to be updated but it wasn't")??;
                anyhow::Result::<(), anyhow::Error>::Ok(())
            }
        };

        Ok((transaction_hash, receipt_future, inclusion_future))
    }
    // endregion:Transaction Execution
}

/// Read each `EventTopic`'s referenced 32-byte topic value out of a receipt's logs.
///
/// Returns a [Vec] aligned by index with `topics`. Errors with a descriptive message if any
/// `log_index` or `topic_index` is out of range — the assumption is that mis-specified indexes
/// in a workload's `variable_assignments` should surface as actionable errors rather than
/// silently produce wrong values.
fn extract_event_topic_values(
    logs: &[alloy::rpc::types::Log],
    topics: &[EventTopic],
) -> Result<Vec<[u8; 32]>> {
    topics
        .iter()
        .map(|et| {
            let log = logs.get(et.log_index).with_context(|| {
                format!(
                    "event_topics: log_index {} out of range (receipt has {} logs)",
                    et.log_index,
                    logs.len(),
                )
            })?;
            let topic = log.topics().get(et.topic_index).with_context(|| {
                format!(
                    "event_topics: topic_index {} out of range in log {} (log has {} topics)",
                    et.topic_index,
                    et.log_index,
                    log.topics().len(),
                )
            })?;
            Ok(topic.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, B256, Bytes};
    use alloy::rpc::types::Log;

    /// Build a synthetic receipt log with the given list of topic bytes. Each entry becomes
    /// one of the log's topics (in order). Address and data are zeroed — neither is read by
    /// the topic-extraction code.
    fn mk_log(topic_bytes: &[[u8; 32]]) -> Log {
        let topics: Vec<B256> = topic_bytes.iter().copied().map(B256::from).collect();
        Log {
            inner: alloy::primitives::Log::new_unchecked(Address::ZERO, topics, Bytes::new()),
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    #[test]
    fn extract_event_topic_values_returns_correct_topics() {
        // Two logs with different topic counts; capture across both.
        let logs = vec![
            mk_log(&[[0x00; 32], [0x11; 32], [0x22; 32]]), // log 0: 3 topics
            mk_log(&[[0xAA; 32], [0xBB; 32]]),             // log 1: 2 topics
        ];
        let topics = vec![
            EventTopic {
                log_index: 0,
                topic_index: 1,
            },
            EventTopic {
                log_index: 1,
                topic_index: 1,
            },
        ];

        let result = extract_event_topic_values(&logs, &topics)
            .expect("happy-path extraction should succeed");

        assert_eq!(result, vec![[0x11; 32], [0xBB; 32]]);
    }

    #[test]
    fn extract_event_topic_values_errors_on_out_of_range_log_index() {
        let logs = vec![mk_log(&[[0x00; 32]])]; // only 1 log
        let topics = vec![EventTopic {
            log_index: 5,
            topic_index: 0,
        }];

        let err = extract_event_topic_values(&logs, &topics)
            .expect_err("out-of-range log_index should error");

        let msg = format!("{err}");
        assert!(
            msg.contains("log_index 5") && msg.contains("receipt has 1 logs"),
            "error message should identify the bad index and actual log count, got: {msg}"
        );
    }

    #[test]
    fn extract_event_topic_values_errors_on_out_of_range_topic_index() {
        let logs = vec![mk_log(&[[0x00; 32], [0x11; 32]])]; // 2 topics
        let topics = vec![EventTopic {
            log_index: 0,
            topic_index: 5,
        }];

        let err = extract_event_topic_values(&logs, &topics)
            .expect_err("out-of-range topic_index should error");

        let msg = format!("{err}");
        assert!(
            msg.contains("topic_index 5") && msg.contains("log has 2 topics"),
            "error message should identify the bad index and actual topic count, got: {msg}"
        );
    }
}
