use crate::internal_prelude::*;

type StepsIterator = std::vec::IntoIter<(StepPath, Step)>;

type CompiledContracts = HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>;

fn next_interpreter_id() -> usize {
    static INTERPRETER_COUNT: AtomicUsize = AtomicUsize::new(0);
    INTERPRETER_COUNT.fetch_add(1, Ordering::Relaxed)
}

pub struct Interpreter<'a, I> {
    interpreter_id: usize,
    platform_information: &'a TestPlatformInformation<'a>,
    test_definition: &'a TestDefinition<'a>,
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
    execution_state: ExecutionState,
    steps_executed: usize,
    steps_iterator: I,
    assertions: Assertions,
    gas_limits: Option<Arc<RwLock<HashMap<StepPath, u64>>>>,
    watcher_tx: Option<UnboundedSender<WatcherEvent>>,
    repetition_count_override: Option<usize>,
    receipt_handling: ReceiptHandling,
    deploy_receipt_timeout: Option<Duration>,
    call_receipt_timeout: Option<Duration>,
}

impl<'a> Interpreter<'a, StepsIterator> {
    pub async fn for_tests(context: InterpreterContext<'a>, steps: StepsIterator) -> Result<Self> {
        Self::new(
            context,
            InterpreterKnobs {
                assertions: Assertions::Enabled,
                gas_limits: None,
                watcher_tx: None,
                repetition_count_override: None,
                receipt_handling: ReceiptHandling::AwaitReceipt,
                deploy_receipt_timeout: None,
                call_receipt_timeout: None,
            },
            steps,
        )
        .await
    }

    pub async fn for_benchmarks(
        context: InterpreterContext<'a>,
        repetition_count_override: Option<usize>,
        watcher_tx: UnboundedSender<WatcherEvent>,
        steps: StepsIterator,
    ) -> Result<Self> {
        Self::new(
            context,
            InterpreterKnobs {
                assertions: Assertions::Disabled,
                gas_limits: Some(Arc::new(Default::default())),
                watcher_tx: Some(watcher_tx),
                repetition_count_override,
                receipt_handling: ReceiptHandling::AwaitReceipt,
                deploy_receipt_timeout: Some(Duration::from_secs(5 * 60)),
                call_receipt_timeout: Some(Duration::from_secs(30 * 60)),
            },
            steps,
        )
        .await
    }
}

impl<'a, I> Interpreter<'a, I>
where
    I: Iterator<Item = (StepPath, Step)>,
{
    pub async fn new(
        context: InterpreterContext<'a>,
        knobs: InterpreterKnobs,
        steps: I,
    ) -> Result<Self> {
        let InterpreterContext {
            platform_information,
            test_definition,
            private_key_allocator,
            cached_compiler,
        } = context;
        let InterpreterKnobs {
            assertions,
            gas_limits,
            watcher_tx,
            repetition_count_override,
            receipt_handling,
            deploy_receipt_timeout,
            call_receipt_timeout,
        } = knobs;
        let execution_state = Self::init_execution_state(
            platform_information,
            test_definition,
            cached_compiler,
            deploy_receipt_timeout,
        )
        .await
        .context("Failed to initialize the execution state of the platform")?;
        Ok(Interpreter {
            interpreter_id: next_interpreter_id(),
            platform_information,
            test_definition,
            private_key_allocator,
            execution_state,
            steps_executed: 0,
            steps_iterator: steps,
            assertions,
            gas_limits,
            watcher_tx,
            repetition_count_override,
            receipt_handling,
            deploy_receipt_timeout,
            call_receipt_timeout,
        })
    }

    async fn init_execution_state(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        cached_compiler: &CachedCompiler<'a>,
        deploy_receipt_timeout: Option<Duration>,
    ) -> Result<ExecutionState> {
        let compiler_output = cached_compiler
            .compile_contracts(
                test_definition.metadata,
                test_definition.metadata_file_path,
                test_definition.mode.clone(),
                None,
                platform_information.compiler.as_ref(),
                platform_information.platform.compiler_identifier(),
                Some(platform_information.platform.platform_identifier()),
                &CompilationReporter::Execution(&platform_information.reporter),
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

        let deployer_address = test_definition.case.deployer_address();

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

            let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                TransactionRequest::default().from(deployer_address),
                code,
            );
            let transaction_hash = platform_information
                .connector
                .send_transaction(tx)
                .await
                .inspect_err(|err| {
                    error!(
                        ?err,
                        %library_instance,
                        platform_identifier = %platform_information.platform.platform_identifier(),
                        "Failed to deploy the library"
                    )
                })?;
            let receipt = match deploy_receipt_timeout {
                Some(duration) => timeout(
                    duration,
                    platform_information.connector.get_receipt(transaction_hash),
                )
                .await
                .context("Failed to get the library deployment receipt within the duration")?
                .context("Failed to get the library deployment receipt")?,
                None => platform_information
                    .connector
                    .get_receipt(transaction_hash)
                    .await
                    .context("Failed to get the library deployment receipt")?,
            };

            if !receipt.status() {
                bail!("Library deployment transaction reverted: {receipt:?}");
            }

            let library_address = receipt
                .contract_address
                .context("Library deployment did not return a contract address")?;

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
                platform_information.platform.compiler_identifier(),
                Some(platform_information.platform.platform_identifier()),
                &CompilationReporter::Execution(&platform_information.reporter),
            )
            .await
            .inspect_err(|err| {
                error!(
                    ?err,
                    platform_identifier = %platform_information.platform.platform_identifier(),
                    "Post-linking compilation failed"
                )
            })
            .context("Failed to compile the post-link contracts")?;

        if platform_information.platform.vm_identifier() == VmIdentifier::PolkaVM {
            let code_upload_tasks = compiler_output
                .contracts
                .values()
                .flat_map(|item| item.values())
                .map(|(code_string, ..)| {
                    alloy::hex::decode(code_string)
                        .context("Failed to decode the contract as hex even after linking")
                        .and_then(|code| {
                            platform_information.connector.code_upload_transaction(code)
                        })
                })
                .collect::<Result<Vec<_>>>()
                .context("Failed to create the code upload transactions")?
                .into_iter()
                .map(|transaction| transaction.from(deployer_address))
                .map(|transaction| async {
                    let receipt = platform_information
                        .connector
                        .execute_transaction(transaction)
                        .await
                        .context("Code upload transaction failed")?;
                    if !receipt.status() {
                        bail!("Code upload transaction reverted: {receipt:?}");
                    }
                    Ok(receipt)
                });
            try_join_all(code_upload_tasks)
                .await
                .context("Some code upload transactions failed")?;
        }

        for (contract_path, contract_name_to_info_mapping) in compiler_output.contracts.iter() {
            for (contract_name, (contract_bytecode, _)) in contract_name_to_info_mapping.iter() {
                let contract_bytecode = hex::decode(contract_bytecode)
                    .context("Failed to hex-decode contract bytecode after linking")?;

                platform_information
                    .reporter
                    .report_contract_information_event(
                        contract_path.to_path_buf(),
                        contract_name.clone(),
                        contract_bytecode.len(),
                    )
                    .context("Failed to report the contract information event")?;
            }
        }

        Ok(ExecutionState::new(
            compiler_output.contracts,
            deployed_libraries.unwrap_or_default(),
        ))
    }

    pub async fn execute_all(mut self) -> Result<ExecuteAllOutcome> {
        while let Some(result) = self.execute_next_step().await {
            result?
        }
        Ok(ExecuteAllOutcome {
            steps_executed: self.steps_executed,
            execution_state: self.execution_state,
        })
    }

    async fn execute_next_step(&mut self) -> Option<Result<()>> {
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
            interpreter_id = self.interpreter_id,
            platform_identifier = %self.platform_information.platform.platform_identifier(),
            node_id = self.platform_information.connector.node_id(),
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
            Step::Transfer(step) => self
                .execute_transfer(step_path, step.as_ref())
                .await
                .context("Transfer Step Failed"),
        }
        .context(format!("Failure on step {step_path}"))?;
        self.steps_executed += steps_executed;
        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id))]
    async fn execute_function_call(
        &mut self,
        step_path: &StepPath,
        step: &FunctionCallStep,
    ) -> Result<usize> {
        let _ = self
            .handle_function_call_contract_deployment(step_path, step)
            .await
            .context("Failed to deploy contracts for the function call step")?;

        let transaction = self
            .build_function_call_transaction_with_context(step, self.default_resolution_context())
            .await
            .context("Failed to create the transaction")?;

        match self.assertions {
            Assertions::Disabled => {
                let TxExecution { tx_hash, receipt } = self
                    .handle_function_call_execution(step_path, transaction, self.receipt_handling)
                    .await
                    .context("Failed to handle the function call execution")?;
                self.record_deployer_execution(step, receipt.as_deref())
                    .await
                    .context("Failed to record deployer execution")?;
                if step.variable_assignments.is_some() {
                    let trace = self
                        .trace_by_hash(tx_hash)
                        .await
                        .context("Failed to trace the transaction for variable assignment")?;
                    self.handle_function_call_variable_assignment(step, &trace)
                        .await
                        .context("Failed to handle function call variable assignment")?;
                }
            }
            Assertions::Enabled => {
                let mut expectations = Self::resolve_expectations(step);
                if let Method::Deployer = &step.method {
                    for expectation in expectations.iter_mut() {
                        expectation.return_data = None;
                    }
                }
                expectations.retain(|expectation| {
                    expectation.compiler_version.as_ref().is_none_or(|version| {
                        version.matches(self.platform_information.compiler.version())
                    })
                });

                let all_expect_failure =
                    !expectations.is_empty() && expectations.iter().all(|e| e.exception);

                if all_expect_failure {
                    let trace_block = self
                        .platform_information
                        .connector
                        .latest_finalized_block()
                        .await
                        .context("Failed to get the block for function call tracing")?;
                    let trace = self
                        .trace_function_call_by_data(step, trace_block.clone())
                        .await
                        .context("Failed to trace the function call by data")?;
                    self.handle_function_call_assertions(
                        &trace,
                        expectations,
                        Some(&trace_block),
                        None,
                    )
                    .await
                    .context("Failed to handle function call assertions")?;
                } else {
                    let TxExecution { tx_hash, receipt } = self
                        .handle_function_call_execution(
                            step_path,
                            transaction,
                            ReceiptHandling::AwaitReceipt,
                        )
                        .await
                        .context("Failed to handle the function call execution")?;
                    self.record_deployer_execution(step, receipt.as_deref())
                        .await
                        .context("Failed to record deployer execution")?;
                    let trace = self
                        .trace_by_hash(tx_hash)
                        .await
                        .context("Failed to trace the executed transaction")?;
                    let trace_block = self
                        .platform_information
                        .connector
                        .transaction_block_pair(tx_hash)
                        .await;
                    self.handle_function_call_variable_assignment(step, &trace)
                        .await
                        .context("Failed to handle function call variable assignment")?;
                    self.handle_function_call_assertions(
                        &trace,
                        expectations,
                        Some(&trace_block),
                        Some(&tx_hash),
                    )
                    .await
                    .context("Failed to handle function call assertions")?;
                }
            }
        }

        Ok(1)
    }

    fn resolve_expectations(step: &FunctionCallStep) -> Vec<ExpectedOutput> {
        match step {
            FunctionCallStep {
                expected: Some(Expected::Calldata(calldata)),
                ..
            } => vec![ExpectedOutput::new().with_calldata(calldata.clone())],
            FunctionCallStep {
                expected: Some(Expected::Expected(expected)),
                ..
            } => vec![expected.clone()],
            FunctionCallStep {
                expected: Some(Expected::ExpectedMany(expected)),
                ..
            } => expected.clone(),
            FunctionCallStep { expected: None, .. } => vec![ExpectedOutput::new().with_success()],
        }
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id))]
    async fn execute_transfer(
        &mut self,
        step_path: &StepPath,
        step: &TransferStep,
    ) -> Result<usize> {
        let context = self.default_resolution_context();
        let from = step
            .from
            .resolve_address(context)
            .await
            .context("Failed to resolve transfer sender")?;
        let to = step
            .to
            .resolve_address(context)
            .await
            .context("Failed to resolve transfer recipient")?;
        let tx = TransactionRequest::default()
            .from(from)
            .to(to)
            .value(step.amount.into_inner());
        self.execute_transaction(
            tx,
            Some(step_path),
            self.receipt_handling,
            self.call_receipt_timeout,
        )
        .await
        .context("Failed to submit transfer transaction")?;
        Ok(1)
    }

    async fn handle_function_call_contract_deployment(
        &mut self,
        step_path: &StepPath,
        step: &FunctionCallStep,
    ) -> Result<HashMap<ContractInstance, Arc<TransactionReceipt>>> {
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
        }

        let mut receipts = HashMap::new();
        for (instance, deploy_with_constructor_arguments) in instances_we_must_deploy.into_iter() {
            let calldata = deploy_with_constructor_arguments.then_some(&step.calldata);
            let value = deploy_with_constructor_arguments
                .then_some(step.value)
                .flatten();

            let caller = {
                let context = self.default_resolution_context();
                step.caller.resolve_address(context).await?
            };
            if let DeploymentOutcome {
                receipt: Some(receipt),
                ..
            } = self
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
        tx: TransactionRequest,
        receipt_handling: ReceiptHandling,
    ) -> Result<TxExecution> {
        self.execute_transaction(
            tx,
            Some(step_path),
            receipt_handling,
            self.call_receipt_timeout,
        )
        .await
        .context("Failed to execute transaction")
    }

    /// Stores the deployment information produced by a successful deployer step.
    ///
    /// The old driver executed `#deployer` steps through the same path as
    /// auto-deployment. The new interpreter executes successful deployer steps
    /// directly so expected-failing deployers can stay on the tracing path. This
    /// recreates the state update that `deploy_contract` would have performed.
    async fn record_deployer_execution(
        &mut self,
        step: &FunctionCallStep,
        receipt: Option<&TransactionReceipt>,
    ) -> Result<()> {
        if !matches!(&step.method, Method::Deployer) {
            return Ok(());
        }

        let receipt = receipt.context("Deployer execution did not return a receipt")?;
        let instance = self
            .default_resolution_context()
            .contract_instance(&step.instance)
            .await
            .context("Failed to resolve the deployer step's instance")?;
        let Some(ContractPathAndIdent {
            contract_source_path,
            contract_ident,
        }) = self
            .test_definition
            .metadata
            .contract_sources()?
            .remove(&instance)
        else {
            bail!("Contract source not found for instance {:?}", instance)
        };

        let compiled_contracts = self.execution_state.compiled_contracts.lock().await;
        let Some(abi) = compiled_contracts
            .get(&contract_source_path)
            .and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
            .map(|(_, abi)| abi.clone())
        else {
            bail!("Failed to find information for contract {:?}", instance)
        };
        drop(compiled_contracts);

        let Some(address) = receipt.contract_address else {
            bail!("Contract deployment didn't return an address");
        };
        info!(
            instance_name = ?instance,
            instance_address = ?address,
            "Deployed contract"
        );
        self.platform_information
            .reporter
            .report_contract_deployed_event(instance.clone(), address)?;

        self.execution_state
            .deployed_contracts
            .insert(instance, Arc::new((contract_ident, address, abi)));

        Ok(())
    }

    async fn build_function_call_transaction_with_context(
        &self,
        step: &FunctionCallStep,
        context: ResolutionContext<'_>,
    ) -> Result<TransactionRequest> {
        match step.method {
            Method::Deployer => self.build_deployer_transaction(step, context).await,
            Method::Fallback | Method::FunctionName(_) => {
                let mut tx = self.build_contract_call_transaction(step, context).await?;
                let gas_overrides = step
                    .gas_overrides
                    .get(&self.platform_information.platform.platform_identifier())
                    .copied()
                    .unwrap_or_default();
                gas_overrides.apply_to::<Ethereum>(&mut tx);
                Ok(tx)
            }
        }
    }

    /// Builds a deployment transaction from compiled bytecode and constructor
    /// calldata.
    async fn build_deployer_transaction(
        &self,
        step: &FunctionCallStep,
        context: ResolutionContext<'_>,
    ) -> Result<TransactionRequest> {
        let instance = context
            .contract_instance(&step.instance)
            .await
            .context("Failed to resolve the step's instance")?;
        let Some(ContractPathAndIdent {
            contract_source_path,
            contract_ident,
        }) = self
            .test_definition
            .metadata
            .contract_sources()?
            .remove(&instance)
        else {
            bail!("Contract source not found for instance {:?}", instance)
        };

        let compiled_contracts = self.execution_state.compiled_contracts.lock().await;
        let Some((code, _)) = compiled_contracts
            .get(&contract_source_path)
            .and_then(|source_file_contracts| source_file_contracts.get(contract_ident.as_ref()))
            .cloned()
        else {
            bail!("Failed to find information for contract {:?}", instance)
        };
        drop(compiled_contracts);

        let mut code = alloy::hex::decode(&code)
            .with_context(|| format!("Failed to hex-decode byte code for {instance:?}"))?;
        code.extend(
            step.calldata
                .calldata(context)
                .await
                .context("Failed to encode constructor calldata")?,
        );

        let caller = step
            .caller
            .resolve_address(context)
            .await
            .context("Failed to resolve deployer caller address")?;
        let tx = TransactionRequest::default().from(caller);
        let tx = match step.value {
            Some(value) => tx.value(value.into_inner()),
            None => tx,
        };
        Ok(TransactionBuilder::<Ethereum>::with_deploy_code(tx, code))
    }

    /// Builds a transaction that calls an already deployed contract instance.
    async fn build_contract_call_transaction(
        &self,
        step: &FunctionCallStep,
        context: ResolutionContext<'_>,
    ) -> Result<TransactionRequest> {
        let input_data = step
            .encoded_input(context)
            .await
            .context("Failed to encode input bytes for transaction request")?;
        let caller = step
            .caller
            .resolve_address(context)
            .await
            .context("Failed to resolve function call caller address")?;
        let to = context
            .deployed_contract_address(&step.instance)
            .await
            .context("Failed to get the contract address")
            .copied()?;
        let tx = TransactionRequest::default().from(caller).value(
            step.value
                .map(|value| value.into_inner())
                .unwrap_or_default(),
        );
        Ok(tx.to(to).input(input_data.into()))
    }

    async fn trace_function_call_by_data(
        &self,
        step: &FunctionCallStep,
        block: BlockPair,
    ) -> Result<CallFrame> {
        let tx = self
            .build_function_call_transaction_with_context(
                step,
                self.default_resolution_context().with_pinned_block(&block),
            )
            .await?;
        let trace = self
            .platform_information
            .connector
            .trace_call(tx, block, Self::call_frame_tracing_options())
            .await?;
        Ok(trace
            .try_into_call_frame()
            .expect("qed; requested call frame trace"))
    }

    async fn trace_by_hash(&self, tx_hash: TxHash) -> Result<CallFrame> {
        let trace = self
            .platform_information
            .connector
            .trace_transaction(tx_hash, Self::call_frame_tracing_options())
            .await?;
        Ok(trace
            .try_into_call_frame()
            .expect("qed; requested call frame trace"))
    }

    fn call_frame_tracing_options() -> GethDebugTracingOptions {
        GethDebugTracingOptions {
            tracer: Some(GethDebugTracerType::BuiltInTracer(
                GethDebugBuiltInTracerType::CallTracer,
            )),
            tracer_config: GethDebugTracerConfig(serde_json::json! {{
                "onlyTopCall": false,
                "withLog": true,
                "withStorage": false,
                "withMemory": false,
                "withStack": false,
                "withReturnData": true
            }}),
            ..Default::default()
        }
    }

    async fn handle_function_call_variable_assignment(
        &mut self,
        step: &FunctionCallStep,
        trace: &CallFrame,
    ) -> Result<()> {
        let Some(ref assignments) = step.variable_assignments else {
            return Ok(());
        };

        for (variable_name, output_word) in assignments.return_data.iter().zip(
            trace
                .output
                .as_ref()
                .map(|output| output.to_vec())
                .unwrap_or_default()
                .chunks(32),
        ) {
            let value = U256::from_be_slice(output_word);
            self.execution_state
                .variables
                .insert(variable_name.clone(), value);
            info!(
                variable_name,
                variable_value = hex::encode(value.to_be_bytes::<32>()),
                "Assigned variable"
            );
        }

        Ok(())
    }

    async fn handle_function_call_assertions(
        &self,
        trace: &CallFrame,
        expectations: Vec<ExpectedOutput>,
        pinned_block: Option<&BlockPair>,
        transaction_hash: Option<&TxHash>,
    ) -> Result<()> {
        let logs = flatten_call_frame_logs(trace);
        futures::stream::iter(expectations.into_iter().map(Ok))
            .try_for_each_concurrent(None, |expectation| {
                let logs = &logs;
                async move {
                    self.handle_function_call_assertion_item(
                        trace,
                        logs,
                        expectation,
                        pinned_block,
                        transaction_hash,
                    )
                    .await
                }
            })
            .await
    }

    async fn handle_function_call_assertion_item(
        &self,
        trace: &CallFrame,
        logs: &[Log],
        assertion: ExpectedOutput,
        pinned_block: Option<&BlockPair>,
        transaction_hash: Option<&TxHash>,
    ) -> Result<()> {
        let resolution_context = self
            .default_resolution_context()
            .with_pinned_block(pinned_block)
            .with_transaction_hash(transaction_hash);

        let expected = !assertion.exception;
        let actual = trace.error.is_none();
        if actual != expected {
            let revert_reason = trace.revert_reason.as_ref().or(trace.error.as_ref());
            error!(
                expected,
                actual,
                ?trace,
                ?revert_reason,
                "Transaction status assertion failed"
            );
            bail!(
                "Transaction status assertion failed - Expected {expected} but got {actual}. Revert reason: {revert_reason:?}",
            );
        }

        if let Some(ref expected_output) = assertion.return_data {
            let actual = trace.output.clone().unwrap_or_default();
            if !expected_output
                .is_equivalent(&actual, resolution_context)
                .await
                .context("Failed to resolve calldata equivalence for return data assertion")?
            {
                error!(?expected_output, %actual, "Output assertion failed");
                bail!("Output assertion failed - Expected {expected_output:?} but got {actual}",);
            }
        }

        if let Some(ref expected_events) = assertion.events {
            let expected = expected_events.len();
            let actual = logs.len();
            if actual != expected {
                error!(expected, actual, "Event count assertion failed");
                bail!("Event count assertion failed - Expected {expected} but got {actual}");
            }

            for (event_idx, (expected_event, actual_event)) in
                expected_events.iter().zip(logs.iter()).enumerate()
            {
                if let Some(ref expected_address) = expected_event.address {
                    let expected = expected_address.resolve_address(resolution_context).await?;
                    let actual = actual_event.address;
                    if actual != expected {
                        error!(event_idx, %expected, %actual, "Event emitter assertion failed");
                        bail!(
                            "Event emitter assertion failed - Expected {expected} but got {actual}",
                        );
                    }
                }

                for (expected, actual) in expected_event
                    .topics
                    .as_slice()
                    .iter()
                    .zip(actual_event.topics())
                {
                    let expected = Calldata::new_compound([expected]);
                    if !expected
                        .is_equivalent(&actual.0, resolution_context)
                        .await
                        .context("Failed to resolve event topic equivalence")?
                    {
                        error!(
                            event_idx,
                            ?expected,
                            ?actual,
                            "Event topics assertion failed"
                        );
                        bail!(
                            "Event topics assertion failed - Expected {expected:?} but got {actual:?}",
                        );
                    }
                }

                let expected = &expected_event.values;
                let actual = &actual_event.data.data;
                if !expected
                    .is_equivalent(&actual.0, resolution_context)
                    .await
                    .context("Failed to resolve event value equivalence")?
                {
                    error!(
                        event_idx,
                        ?expected,
                        ?actual,
                        "Event value assertion failed"
                    );
                    bail!(
                        "Event value assertion failed - Expected {expected:?} but got {actual:?}",
                    );
                }
            }
        }

        Ok(())
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id), err(Debug))]
    async fn execute_balance_assertion(
        &mut self,
        _: &StepPath,
        step: &BalanceAssertionStep,
    ) -> Result<usize> {
        self.step_address_auto_deployment(&step.address)
            .await
            .context("Failed to perform auto-deployment for the step address")?;

        let address = step
            .address
            .resolve_address(self.default_resolution_context())
            .await?;

        let balance = self
            .platform_information
            .connector
            .balance_of(address)
            .await?;

        let expected = step.expected_balance;
        let actual = balance;
        if expected != actual {
            error!(%expected, %actual, %address, "Balance assertion failed");
            bail!(
                "Balance assertion failed - Expected {} but got {} for {} resolved to {}",
                expected,
                actual,
                address,
                address,
            )
        }

        Ok(1)
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id), err(Debug))]
    async fn execute_storage_empty_assertion_step(
        &mut self,
        _: &StepPath,
        _: &StorageEmptyAssertionStep,
    ) -> Result<usize> {
        warn!("Storage Empty Assertions are not supported in retester");
        Ok(1)
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id), err(Debug))]
    async fn execute_repeat_step(
        &mut self,
        step_path: &StepPath,
        step: &RepeatStep,
    ) -> Result<usize> {
        let repeat_count = self.repetition_count_override.unwrap_or(step.repeat);
        let warm_up = self.gas_limits.is_some();

        let build_child = |i: usize| {
            let mut execution_state = self.execution_state.clone();
            if let Some(index_variable) = step
                .capture_index
                .as_ref()
                .and_then(|capture_index| capture_index.strip_prefix("$VARIABLE:"))
            {
                execution_state
                    .variables
                    .insert(index_variable.to_string(), U256::from(i));
            }

            let steps = step
                .steps
                .iter()
                .cloned()
                .enumerate()
                .map(|(step_idx, step)| {
                    let step_path = step_path.append(StepIdx::new(step_idx));
                    (step_path, step)
                })
                .collect::<Vec<_>>();

            let receipt_handling = match (warm_up, self.receipt_handling, i) {
                (true, ReceiptHandling::AwaitReceipt, 0) => ReceiptHandling::AwaitReceipt,
                (true, _, _) => ReceiptHandling::FireAndForget,
                (false, receipt_handling, _) => receipt_handling,
            };

            Interpreter {
                interpreter_id: next_interpreter_id(),
                platform_information: self.platform_information,
                test_definition: self.test_definition,
                private_key_allocator: self.private_key_allocator.clone(),
                execution_state,
                steps_executed: 0,
                steps_iterator: steps.into_iter(),
                assertions: self.assertions,
                gas_limits: self.gas_limits.clone(),
                watcher_tx: self.watcher_tx.clone(),
                repetition_count_override: self.repetition_count_override,
                receipt_handling,
                deploy_receipt_timeout: self.deploy_receipt_timeout,
                call_receipt_timeout: self.call_receipt_timeout,
            }
        };

        let run_child =
            |i: usize| -> Pin<Box<dyn Future<Output = Result<ExecuteAllOutcome>> + '_>> {
                Box::pin(build_child(i).execute_all())
            };

        if warm_up {
            let mut children = (0..repeat_count).map(run_child);

            let Some(first_child) = children.next() else {
                return Ok(0);
            };
            let first_outcome = first_child
                .await
                .context("Running the first initialization interpreter failed")?;

            if step.start_watcher
                && let Some(watcher_tx) = self.watcher_tx.as_ref()
            {
                watcher_tx
                    .send(WatcherEvent::StartEvent {
                        ignore_block_before: self
                            .platform_information
                            .connector
                            .latest_finalized_block()
                            .await
                            .context("Failed to get the latest block")?
                            .evm_block
                            .number(),
                    })
                    .context("Failed to send message on the watcher's tx")?;
            }

            let outcomes = try_join_all(children)
                .await
                .context("Repetition execution failed")?;

            let steps_executed = first_outcome.steps_executed
                + outcomes
                    .iter()
                    .map(|outcome| outcome.steps_executed)
                    .sum::<usize>();

            if step.consolidate_state {
                self.execution_state
                    .add_state(first_outcome.execution_state);
                for outcome in outcomes {
                    self.execution_state.add_state(outcome.execution_state);
                }
            }

            Ok(steps_executed)
        } else {
            let outcomes = try_join_all((0..repeat_count).map(run_child))
                .await
                .context("Repetition execution failed")?;

            let steps_executed = outcomes
                .iter()
                .map(|outcome| outcome.steps_executed)
                .sum::<usize>();

            if step.consolidate_state {
                for outcome in outcomes {
                    self.execution_state.add_state(outcome.execution_state);
                }
            }

            Ok(steps_executed)
        }
    }

    #[instrument(level = "info", skip_all, fields(interpreter_id = self.interpreter_id), err(Debug))]
    async fn execute_account_allocation(
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

    #[instrument(
        level = "info",
        skip_all,
        fields(interpreter_id = self.interpreter_id, %contract_instance, %deployer),
        err(Debug),
    )]
    async fn get_or_deploy_contract_instance(
        &mut self,
        contract_instance: &ContractInstance,
        deployer: Address,
        calldata: Option<&Calldata>,
        value: Option<EtherValue>,
        step_path: Option<&StepPath>,
    ) -> Result<DeploymentOutcome> {
        if let Some(entry) = self
            .execution_state
            .deployed_contracts
            .get(contract_instance)
        {
            let (_, address, _) = entry.as_ref();
            info!(%address, "Contract instance already deployed.");
            Ok(DeploymentOutcome {
                address: *address,
                receipt: None,
            })
        } else {
            info!("Contract instance requires deployment.");
            let outcome = self
                .deploy_contract(contract_instance, deployer, calldata, value, step_path)
                .await
                .context("Failed to deploy contract")?;
            info!(address = %outcome.address, "Contract instance has been deployed.");
            Ok(outcome)
        }
    }

    #[instrument(
        level = "info",
        skip_all,
        fields(interpreter_id = self.interpreter_id, %contract_instance, %deployer),
        err(Debug),
    )]
    async fn deploy_contract(
        &mut self,
        contract_instance: &ContractInstance,
        deployer: Address,
        calldata: Option<&Calldata>,
        value: Option<EtherValue>,
        step_path: Option<&StepPath>,
    ) -> Result<DeploymentOutcome> {
        let Some(ContractPathAndIdent {
            contract_source_path,
            contract_ident,
        }) = self
            .test_definition
            .metadata
            .contract_sources()?
            .remove(contract_instance)
        else {
            bail!(
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
            bail!(
                "Failed to find information for contract {:?}",
                contract_instance
            )
        };
        drop(compiled_contracts);

        let mut code = match alloy::hex::decode(&code) {
            Ok(code) => code,
            Err(error) => {
                error!(
                    ?error,
                    contract_source_path = contract_source_path.display().to_string(),
                    contract_ident = contract_ident.as_ref(),
                    "Failed to hex-decode byte code - This could possibly mean that the bytecode requires linking"
                );
                bail!("Failed to hex-decode the byte code {}", error)
            }
        };

        if let Some(calldata) = calldata {
            let calldata = calldata.calldata(self.default_resolution_context()).await?;
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

        let receipt = self
            .execute_transaction(
                tx,
                step_path,
                ReceiptHandling::AwaitReceipt,
                self.deploy_receipt_timeout,
            )
            .await
            .inspect_err(|error| error!(?error, "Contract deployment transaction failed."))?
            .receipt
            .context("Deployment awaited the receipt but none was returned")?;

        let Some(address) = receipt.contract_address else {
            bail!("Contract deployment didn't return an address");
        };
        info!(
            instance_name = ?contract_instance,
            instance_address = ?address,
            "Deployed contract"
        );
        self.platform_information
            .reporter
            .report_contract_deployed_event(contract_instance.clone(), address)?;

        self.execution_state.deployed_contracts.insert(
            contract_instance.clone(),
            Arc::new((contract_ident, address, abi)),
        );

        Ok(DeploymentOutcome {
            address,
            receipt: Some(receipt),
        })
    }

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
                    None,
                )
                .await
                .map(|outcome| outcome.address)
            }
        }
    }

    #[instrument(
        level = "info",
        skip_all,
        fields(interpreter_id = self.interpreter_id, transaction_hash = tracing::field::Empty),
        err(Debug),
    )]
    async fn execute_transaction(
        &self,
        mut transaction: TransactionRequest,
        step_path: Option<&StepPath>,
        receipt_handling: ReceiptHandling,
        receipt_timeout: Option<Duration>,
    ) -> Result<TxExecution> {
        let connector = self.platform_information.connector.clone();

        if let (Some(gas_limits), Some(step_path)) = (self.gas_limits.as_ref(), step_path)
            && self.platform_information.platform.allow_caching_gas_limit()
        {
            let read_guard = gas_limits.read().await;
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
                    let mut write_guard = gas_limits.write().await;
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
                                    match connector.estimate_gas(transaction.clone()).await {
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

        let transaction_hash = connector
            .send_transaction(transaction)
            .await
            .context("Failed to submit transaction")?;
        Span::current().record("transaction_hash", display(transaction_hash));

        info!(%transaction_hash, "Submitted transaction");
        if let (Some(watcher_tx), Some(step_path)) = (self.watcher_tx.as_ref(), step_path) {
            watcher_tx
                .send(WatcherEvent::SubmittedTransaction {
                    transaction_hash,
                    step_path: step_path.clone(),
                    submission_time: SystemTime::now(),
                })
                .context("Failed to send the transaction hash to the watcher")?;
        }

        let receipt = match (receipt_handling, receipt_timeout) {
            (ReceiptHandling::AwaitReceipt, Some(duration)) => Some(
                timeout(duration, connector.get_receipt(transaction_hash))
                    .await
                    .context("Failed to get the receipt within the allocated duration")?
                    .context("Failed to get the receipt")?,
            ),
            (ReceiptHandling::AwaitReceipt, None) => Some(
                connector
                    .get_receipt(transaction_hash)
                    .await
                    .context("Failed to get the receipt")?,
            ),
            (ReceiptHandling::FireAndForget, _) => None,
        };

        if let Some(ref receipt) = receipt
            && !receipt.status()
        {
            bail!("Submitted transaction failed: {receipt:?}");
        }

        Ok(TxExecution {
            tx_hash: transaction_hash,
            receipt,
        })
    }

    fn default_resolution_context(&self) -> ResolutionContext<'_> {
        ResolutionContext::default()
            .with_deployed_contracts(&self.execution_state.deployed_contracts)
            .with_variables(&self.execution_state.variables)
            .with_metadata(&self.test_definition.metadata.content)
            .with_node_connector(&*self.platform_information.connector)
    }
}

pub struct InterpreterContext<'a> {
    pub platform_information: &'a TestPlatformInformation<'a>,
    pub test_definition: &'a TestDefinition<'a>,
    pub private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
    pub cached_compiler: &'a CachedCompiler<'a>,
}

pub struct InterpreterKnobs {
    pub assertions: Assertions,
    pub gas_limits: Option<Arc<RwLock<HashMap<StepPath, u64>>>>,
    pub watcher_tx: Option<UnboundedSender<WatcherEvent>>,
    pub repetition_count_override: Option<usize>,
    pub receipt_handling: ReceiptHandling,
    pub deploy_receipt_timeout: Option<Duration>,
    pub call_receipt_timeout: Option<Duration>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Assertions {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReceiptHandling {
    AwaitReceipt,
    FireAndForget,
}

struct TxExecution {
    tx_hash: TxHash,
    receipt: Option<Arc<TransactionReceipt>>,
}

struct DeploymentOutcome {
    address: Address,
    receipt: Option<Arc<TransactionReceipt>>,
}

pub struct ExecuteAllOutcome {
    pub steps_executed: usize,
    pub execution_state: ExecutionState,
}

#[derive(Clone)]
pub struct ExecutionState {
    pub compiled_contracts: Arc<tokio::sync::Mutex<CompiledContracts>>,
    pub deployed_contracts: HashMap<ContractInstance, Arc<(ContractIdent, Address, JsonAbi)>>,
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

fn flatten_call_frame_logs(frame: &CallFrame) -> Vec<Log> {
    let mut logs = Vec::new();
    collect_call_frame_logs(frame, &mut logs);
    logs
}

fn collect_call_frame_logs(frame: &CallFrame, logs: &mut Vec<Log>) {
    if frame.error.is_some() || frame.revert_reason.is_some() {
        return;
    }

    let mut next_child = 0;
    for log in frame.logs.iter() {
        let position = log.position.unwrap_or_default() as usize;
        while next_child < frame.calls.len() && next_child < position {
            collect_call_frame_logs(&frame.calls[next_child], logs);
            next_child += 1;
        }
        logs.push(call_log_frame_to_log(log));
    }
    while next_child < frame.calls.len() {
        collect_call_frame_logs(&frame.calls[next_child], logs);
        next_child += 1;
    }
}

fn call_log_frame_to_log(log: &CallLogFrame) -> Log {
    Log::new_unchecked(
        log.address.unwrap_or_default(),
        log.topics.clone().unwrap_or_default(),
        log.data.clone().unwrap_or_default(),
    )
}
