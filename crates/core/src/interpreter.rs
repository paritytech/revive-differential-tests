use alloy::consensus::{Transaction, transaction::SignerRecoverable};
use futures::stream::FuturesUnordered;

use crate::internal_prelude::*;

type StepsIterator = std::vec::IntoIter<(StepPath, Step)>;

fn next_interpreter_id() -> usize {
    static INTERPRETER_COUNT: AtomicUsize = AtomicUsize::new(0);
    INTERPRETER_COUNT.fetch_add(1, Ordering::Relaxed)
}

#[allow(clippy::type_complexity)]
pub struct Interpreter<'a> {
    id: usize,
    platform_information: &'a TestPlatformInformation<'a>,
    test_definition: &'a TestDefinition<'a>,
    connector: Arc<NodeConnector>,
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
    watcher_tx: Option<UnboundedSender<WatcherEvent>>,
    repetition_count_override: Option<usize>,
    /* State */
    step_state: Option<StepState<'a>>,
    variables: HashMap<String, LazyFutureValue<'static, Result<U256>>>,
    deployed_contracts:
        HashMap<ContractInstance, (LazyFutureValue<'static, Result<Address>>, Arc<JsonAbi>)>,
    compiled_contracts: Arc<HashMap<PathBuf, HashMap<String, (Arc<Vec<u8>>, Arc<JsonAbi>)>>>,
    gas_cache: Option<Arc<DashMap<StepPath, Arc<OnceCell<u64>>>>>,
    steps: StepsIterator,
    tasks: Vec<JoinHandle<()>>,
    /* Configuration */
    await_receipts: bool,
    run_assertions: bool,
    run_reporter: bool,
}

struct StepState<'a> {
    step_path: StepPath,
    merge_forked_interpreters: bool,
    forked_interpreters: Vec<Interpreter<'a>>,
}

impl<'a> Interpreter<'a> {
    pub async fn new(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        cached_compiler: &CachedCompiler<'a>,
        steps: StepsIterator,
    ) -> Result<Self> {
        Self {
            id: next_interpreter_id(),
            connector: platform_information.connector.clone(),
            platform_information,
            test_definition,
            private_key_allocator,
            steps,
            watcher_tx: Default::default(),
            repetition_count_override: Default::default(),
            step_state: Default::default(),
            variables: Default::default(),
            deployed_contracts: Default::default(),
            compiled_contracts: Default::default(),
            gas_cache: Default::default(),
            tasks: Default::default(),
            await_receipts: Default::default(),
            run_assertions: Default::default(),
            run_reporter: true,
        }
        .initialize(cached_compiler)
        .inspect(|_| info!("Interpreter is created and ready"))
        .await
    }

    pub async fn for_tests(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        cached_compiler: &CachedCompiler<'a>,
        steps: StepsIterator,
    ) -> Result<Self> {
        Self::new(
            platform_information,
            test_definition,
            private_key_allocator,
            cached_compiler,
            steps,
        )
        .await
        .map(|this| {
            this.with_gas_caching(false)
                .with_await_receipts(true)
                .with_run_assertions(true)
        })
    }

    pub async fn for_benchmarks(
        platform_information: &'a TestPlatformInformation<'a>,
        test_definition: &'a TestDefinition<'a>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        watcher_tx: UnboundedSender<WatcherEvent>,
        cached_compiler: &CachedCompiler<'a>,
        steps: StepsIterator,
    ) -> Result<Self> {
        Self::new(
            platform_information,
            test_definition,
            private_key_allocator,
            cached_compiler,
            steps,
        )
        .await
        .map(|this| {
            this.with_watcher(watcher_tx)
                .with_gas_caching(true)
                .with_await_receipts(true)
                .with_run_assertions(false)
        })
    }

    async fn initialize(mut self, cached_compiler: &CachedCompiler<'a>) -> Result<Self> {
        let pre_link_compilation_output = self
            .compile_contracts(cached_compiler, None)
            .await
            .context("Pre-link compilation failed")?;

        let deployer = self.test_definition.case.deployer_address();
        let mut contract_sources = self
            .test_definition
            .metadata
            .contract_sources()
            .context("Failed to get the contract sources")?;
        let library_instances = self
            .test_definition
            .metadata
            .libraries
            .iter()
            .flatten()
            .flat_map(|(_, map)| map.values());

        let mut deployed_libraries = HashMap::new();
        for library_instance in library_instances {
            let source_and_ident = contract_sources
                .remove(library_instance)
                .context("Library instance isn't defined in the metadata file")?;
            let (code, abi) = pre_link_compilation_output
                .contracts
                .get(&source_and_ident.contract_source_path)
                .and_then(|map| map.get(source_and_ident.contract_ident.as_inner().as_str()))
                .context("Could not locate library code in the compilation artifacts")?;
            let code = alloy::hex::decode(code).context("Library code must be hex decodable")?;
            let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                TransactionRequest::default(),
                code,
            )
            .from(deployer);
            let (tx_hash, receipt) = self
                .execute_transaction(tx)
                .await
                .context("Transaction execution failed")?;
            info!(?library_instance, ?tx_hash, "Deployed library");
            let receipt = timeout(Duration::from_secs(5 * 60), async move {
                receipt.get_owned_error().await.cloned()
            })
            .await
            .context("Library deployment timed-out")?
            .context("Failed to get the receipt for library deployment")?;
            ensure!(
                receipt.status(),
                "Library deployment transaction failed. Receipt = {:?}",
                receipt
            );

            let library_address = receipt
                .contract_address
                .expect("qed; this is a deployment tx");
            deployed_libraries.insert(
                library_instance.clone(),
                (
                    source_and_ident.contract_ident,
                    library_address,
                    abi.clone(),
                ),
            );
        }

        if !deployed_libraries.is_empty() && self.run_reporter {
            self.platform_information
                .reporter
                .report_libraries_deployed_event(
                    deployed_libraries
                        .iter()
                        .map(|(instance, (_, address, _))| (instance.clone(), *address))
                        .collect::<BTreeMap<_, _>>(),
                )
                .context("Failed to report the deployed libraries event")?;
        }

        let post_link_compilation_output = self
            .compile_contracts(cached_compiler, Some(&deployed_libraries))
            .await
            .context("Post link compilation failed")?;

        self.compiled_contracts = post_link_compilation_output
            .contracts
            .into_iter()
            .map(|(path, ident_artifacts_mapping)| {
                let map = ident_artifacts_mapping
                    .into_iter()
                    .map(|(ident, (code, abi))| {
                        let code = alloy::hex::decode(code)
                            .context("Compiled code must be hex decodable")?;
                        if self.run_reporter {
                            self.platform_information
                                .reporter
                                .report_contract_information_event(
                                    path.clone(),
                                    ident.as_str(),
                                    code.len(),
                                )
                                .expect("fatal: reporter configured but unreachable");
                        }
                        Ok((ident, (Arc::new(code), Arc::new(abi))))
                    })
                    .collect::<Result<HashMap<_, _>>>()?;
                Ok((path, map))
            })
            .collect::<Result<HashMap<_, _>>>()
            .map(Arc::new)?;
        self.deployed_contracts = deployed_libraries
            .into_iter()
            .map(|(instance, (_, address, json_abi))| {
                (
                    instance,
                    (
                        LazyFutureValue::new(move || ready(Ok(address))),
                        Arc::new(json_abi),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();

        if self.platform_information.platform.vm_identifier() == VmIdentifier::PolkaVM {
            let code_upload_tasks = self
                .compiled_contracts
                .values()
                .flat_map(|item| item.values())
                .map(|(code, ..)| self.connector.code_upload_transaction(code.as_slice()))
                .collect::<Result<Vec<_>>>()
                .context("Failed to create the code upload transactions")?
                .into_iter()
                .map(|transaction| transaction.from(deployer))
                .map(|transaction| async {
                    let receipt = self
                        .connector
                        .execute_transaction(transaction)
                        .await
                        .context("Code upload transaction failed")?;
                    if !receipt.status() {
                        bail!("Code upload transaction reverted: {receipt:?}");
                    }
                    Ok(receipt)
                });
            let mut code_upload_tasks = code_upload_tasks.collect::<FuturesUnordered<_>>();
            while let Some(receipt) = code_upload_tasks.next().await {
                receipt.context("Some code upload transactions failed")?;
            }
        }

        Ok(self)
    }

    async fn compile_contracts(
        &self,
        cached_compiler: &CachedCompiler<'a>,
        deployed_libraries: Option<&HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,
    ) -> Result<CompilerOutput> {
        cached_compiler
            .compile_contracts(
                self.test_definition.metadata,
                self.test_definition.metadata_file_path,
                self.test_definition.mode.clone(),
                deployed_libraries,
                self.platform_information.compiler.as_ref(),
                self.platform_information.platform.compiler_identifier(),
                &CompilationReporter::Execution(&self.platform_information.reporter),
            )
            .await
    }

    pub fn with_watcher(
        self,
        watcher_tx: impl Into<Option<UnboundedSender<WatcherEvent>>>,
    ) -> Self {
        self.with_mutator(|this| this.watcher_tx = watcher_tx.into())
    }

    pub fn with_repetition_count_override(self, value: impl Into<Option<usize>>) -> Self {
        self.with_mutator(|this| this.repetition_count_override = value.into())
    }

    pub fn with_gas_caching(self, value: bool) -> Self {
        self.with_mutator(|this| this.gas_cache = value.then(Default::default))
    }

    pub fn with_await_receipts(self, value: bool) -> Self {
        self.with_mutator(|this| this.await_receipts = value)
    }

    pub fn with_run_assertions(self, value: bool) -> Self {
        self.with_mutator(|this| this.run_assertions = value)
    }

    fn with_mutator(mut self, mutator: impl FnOnce(&mut Self)) -> Self {
        mutator(&mut self);
        self
    }

    pub async fn run_to_completion(self) -> Result<()> {
        self.handle_gas_warm_up()
            .await
            .context("Failed to handle gas warm up run")?;
        let this = self.internal_run_to_completion().await?;
        if let Some(watcher) = this.watcher_tx {
            watcher
                .send(WatcherEvent::AllTransactionsSubmitted)
                .expect("fatal: watcher available but its channel is closed");
        }
        info!(
            len = this.tasks.len(),
            "Awaiting all of the side effect tasks to complete"
        );
        join_all(this.tasks).await;
        info!("Execution completed");
        Ok(())
    }

    async fn handle_gas_warm_up(&self) -> Result<()> {
        if self.gas_cache.is_none() {
            return Ok(());
        }

        fn edit_step_receipt_behavior<'a>(iter: impl IntoIterator<Item = &'a mut Step>) {
            for step in iter.into_iter() {
                if let Step::Repeat(step) = step {
                    step.await_transaction_inclusion = true;
                    edit_step_receipt_behavior(step.steps.iter_mut());
                }
            }
        }
        let mut steps = self.steps.clone().collect::<Vec<_>>();
        edit_step_receipt_behavior(steps.iter_mut().map(|v| &mut v.1));

        let fork = Self {
            // Reserved for special warm up interpreter
            id: usize::MAX,
            platform_information: self.platform_information,
            test_definition: self.test_definition,
            connector: self.connector.clone(),
            // Forked allocator so that we don't consume any of the keys we had allocated and run
            // out of them earlier than we anticipated.
            private_key_allocator: Arc::new(Mutex::new(
                self.private_key_allocator.lock().await.fork(),
            )),
            watcher_tx: None,
            // Explicit count override of 1 so that we run the entire workload as a flat sequence of
            // instructions.
            repetition_count_override: Some(1),
            step_state: None,
            variables: self.variables.clone(),
            deployed_contracts: self.deployed_contracts.clone(),
            compiled_contracts: self.compiled_contracts.clone(),
            gas_cache: self.gas_cache.clone(),
            steps: steps.into_iter(),
            tasks: vec![],
            await_receipts: true,
            run_assertions: false,
            run_reporter: false,
        };
        fork.internal_run_to_completion()
            .await
            .context("Failed to run the warm up fork to completion")?;

        Ok(())
    }

    #[instrument(
        name = "Executing Step",
        level = "debug",
        skip_all,
        fields(
            int_id = %format_args!("{:#x}", self.id),
            step_path = tracing::field::Empty,
        )
    )]
    async fn internal_run_to_completion(mut self) -> Result<Self> {
        while let Some((step_path, step)) = self.steps.next() {
            Span::current().record("step_path", tracing::field::display(&step_path));
            debug!("Starting step execution");
            assert!(
                self.step_state.is_none(),
                "This is a bug, the step state must be none before executing a step"
            );
            self.step_state = Some(StepState {
                step_path: step_path.clone(),
                merge_forked_interpreters: false,
                forked_interpreters: vec![],
            });

            let executable = match step {
                Step::FunctionCall(step) => {
                    let from = step
                        .caller
                        .resolve_address(&mut self.default_resolution_context())
                        .await
                        .context("Failed to resolve the caller address")?;
                    let calldata = step
                        .calldata
                        .calldata(&mut self.default_resolution_context())
                        .await
                        .context("Failed to resolve calldata")?;

                    let mut expectations = match *step {
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
                        FunctionCallStep { expected: None, .. } => {
                            vec![ExpectedOutput::new().with_success()]
                        }
                    };
                    if let Method::Deployer = &step.method {
                        for expectation in expectations.iter_mut() {
                            expectation.return_data = None;
                        }
                    }
                    let expectations = expectations
                        .into_iter()
                        .filter(|expectation| {
                            expectation.compiler_version.as_ref().is_none_or(|version| {
                                version.matches(self.platform_information.compiler.version())
                            })
                        })
                        .collect();

                    TransactionExecutableStep {
                        from,
                        to: step.instance,
                        value: step
                            .value
                            .map(|value| value.into_inner())
                            .unwrap_or_default(),
                        method: step.method,
                        calldata: calldata.into(),
                        expectations,
                        variable_assignment: step.variable_assignments,
                        gas_override: step
                            .gas_overrides
                            .get(&self.platform_information.platform.platform_identifier())
                            .copied(),
                    }
                    .into()
                }
                Step::BalanceAssertion(step) => {
                    let address = step
                        .address
                        .resolve_address(&mut self.default_resolution_context())
                        .await
                        .context("Failed to resolve assertion address")?;
                    BalanceAssertionExecutableStep {
                        address,
                        balance: step.expected_balance,
                    }
                    .into()
                }
                Step::StorageEmptyAssertion(step) => {
                    let address = step
                        .address
                        .resolve_address(&mut self.default_resolution_context())
                        .await
                        .context("Failed to resolve assertion address")?;
                    StorageContentAssertionExecutableStep {
                        address,
                        is_storage_empty: step.is_storage_empty,
                    }
                    .into()
                }
                Step::Repeat(step) => {
                    if let Some(ref watcher) = self.watcher_tx
                        && step.start_watcher
                    {
                        let current_block = self.connector.latest_block().await?;
                        watcher
                            .send(WatcherEvent::StartEvent {
                                ignore_block_before: current_block.evm_block.number(),
                            })
                            .expect("fatal: watcher is being used but its channel is destroyed")
                    }
                    RepetitionExecutableStep {
                        repetition_count: step.repeat,
                        await_receipts: step.await_transaction_inclusion,
                        consolidate_state: step.consolidate_state,
                        capture_index: step.capture_index,
                        steps: step.steps,
                    }
                    .into()
                }
                Step::AllocateAccount(step) => AllocateAccountExecutableStep {
                    variable_name: step.variable_name,
                }
                .into(),
                Step::Transfer(step) => {
                    let from = step
                        .from
                        .resolve_address(&mut self.default_resolution_context())
                        .await
                        .context("from address resolution failed")?;
                    let to = step
                        .to
                        .resolve_address(&mut self.default_resolution_context())
                        .await
                        .context("to address resolution failed")?;
                    let amount = step.amount.into_inner();
                    TransferExecutableStep { from, to, amount }.into()
                }
            };
            self.executable_step_loop(executable)
                .instrument(info_span!("Executing step", step = %step_path))
                .await
                .with_context(|| format!("Step {step_path} execution failed"))?;

            assert!(
                self.step_state.is_some(),
                "This is a bug, the step state must be some after executing a step"
            );
            self.step_state = None;
            debug!("Finished step execution");
        }
        Ok(self)
    }

    async fn executable_step_loop(&mut self, step: AnyExecutableStep) -> Result<()> {
        let mut steps = std::iter::once(step).collect::<VecDeque<_>>();
        while let Some(step) = steps.pop_front() {
            trace!(steps = steps.len(), "Executing step");
            let outcome = step.execute(self).await.context("Step execution failed")?;
            steps.extend(outcome.next_steps);
            self.handle_forks()
                .await
                .context("Failed to handle interpreter forks")?;
        }
        Ok(())
    }

    async fn handle_forks(&mut self) -> Result<()> {
        let Some(StepState {
            forked_interpreters: ref mut forks,
            merge_forked_interpreters,
            ..
        }) = self.step_state
        else {
            panic!("This is a bug: step execution without step state")
        };
        if !forks.is_empty() {
            debug!(len = forks.len(), "Running forks");
        }

        let mut fork_tasks = forks
            .drain(..)
            .map(|fork| fork.internal_run_to_completion())
            .collect::<FuturesUnordered<_>>();
        while let Some(fork) = fork_tasks.next().await {
            let fork = fork.context("Some interpreter fork failed")?;
            self.tasks.extend(fork.tasks);

            if merge_forked_interpreters {
                self.variables.extend(fork.variables);
                self.deployed_contracts.extend(fork.deployed_contracts);
            }
        }
        Ok(())
    }

    fn default_resolution_context<'b>(&'b mut self) -> ResolutionContext<'b, Self> {
        ResolutionContext {
            metadata: Some(self.test_definition.metadata),
            pinned_block: None,
            transaction_hash: None,
            node_connector: Some(self.connector.clone()),
            api: Some(self),
        }
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
}

impl<'a> InterpreterApi for Interpreter<'a> {
    async fn execute_transaction(
        &mut self,
        mut tx: TransactionRequest,
    ) -> Result<(
        TxHash,
        LazyFutureValue<'static, Result<Arc<TransactionReceipt>>>,
    )> {
        let gas_cache_cell =
            self.step_state
                .as_ref()
                .zip(self.gas_cache.as_ref())
                .map(|(step_state, gas_cache)| {
                    gas_cache
                        .entry(step_state.step_path.clone())
                        .or_default()
                        .clone()
                });
        if let Some(gas_cache_cell) = gas_cache_cell {
            debug!("Going through cache path for gas");
            let value = gas_cache_cell
                .get_or_try_init(|| async {
                    debug!("Gas estimate is a cache miss");
                    self.connector
                        .estimate_gas(tx.clone())
                        .await
                        .context("Gas estimation failed")
                        // TODO: Can I lower this a little bit?
                        .map(|value| value * 120 / 100)
                })
                .await?;
            tx = tx.gas_limit(*value);
        }

        let tx_hash = self
            .connector
            .send_transaction(tx)
            .await
            .context("Failed to send transaction")?;
        if let (Some(watcher_tx), Some(step_state)) =
            (self.watcher_tx.as_ref(), self.step_state.as_ref())
        {
            watcher_tx
                .send(WatcherEvent::SubmittedTransaction {
                    transaction_hash: tx_hash,
                    step_path: step_state.step_path.clone(),
                    submission_time: SystemTime::now(),
                })
                .expect("fatal: configured with watcher but can't send watcher events");
        }

        let connector = self.connector.clone();
        let receipt_value = LazyFutureValue::new(move || {
            let connector = connector.clone();
            async move {
                let receipt = connector.get_receipt(tx_hash).await?;
                ensure!(
                    receipt.status(),
                    "Requested a receipt, but its transaction failed. Receipt = {:?}",
                    receipt
                );
                Ok::<_, anyhow::Error>(receipt)
            }
        });
        if self.await_receipts {
            debug!("Interpreter configured to await for the receipt value");
            let receipt = receipt_value
                .get_owned_error()
                .await
                .context("Failed to get the receipt")?;
            if !receipt.status() {
                bail!("Transaction failed: {receipt:?}");
            }
        }
        Ok((tx_hash, receipt_value))
    }

    async fn allocate_account(&mut self) -> LazyFutureValue<'static, Result<Address>> {
        let allocator = self.private_key_allocator.clone();
        LazyFutureValue::new(move || {
            let allocator = allocator.clone();
            async move {
                let mut allocator = allocator.lock().await;
                allocator.allocate().map(|key| key.address())
            }
        })
    }

    async fn resolve_variable_name(&mut self, raw_name: &str) -> Result<String> {
        if raw_name.contains("$VARIABLE:") {
            let mut context = self.default_resolution_context();
            CalldataToken::<&str>::resolve_variable_name_template(raw_name, &mut context)
                .await
                .context("Failed to resolve variable name template")
        } else {
            Ok(raw_name.to_owned())
        }
    }

    async fn run_assertion(&mut self, assertion: ExecutableAssertion) -> Result<()> {
        if !self.run_assertions {
            return Ok(());
        }

        let (assertions, block, trace, maybe_receipt) = match assertion {
            ExecutableAssertion::TransactionAssertionsForExpectedSuccess {
                assertions,
                receipt,
                trace,
            } => {
                let trace = trace
                    .get_owned_error()
                    .await
                    .context("Failed to get the trace")?;
                let receipt = receipt
                    .get_owned_error()
                    .await
                    .context("Failed to get the trace")?;
                let block = self
                    .connector
                    .block(
                        receipt
                            .block_number
                            .expect("qed; a receipt always has a block number"),
                    )
                    .await;
                (assertions, block, trace.clone(), Some(receipt.clone()))
            }
            ExecutableAssertion::TransactionAssertionsForExpectedFailure { assertions, trace } => {
                let (traced_at_block, trace) = trace
                    .get_owned_error()
                    .await
                    .context("Failed to get the trace for a failing transaction")?;
                (assertions, traced_at_block.clone(), trace.clone(), None)
            }
            ExecutableAssertion::BalanceAssertion {
                address,
                balance: expected_balance,
            } => {
                let current_balance = self
                    .connector
                    .balance_of(address)
                    .await
                    .context("Failed to get the balance of the address")?;
                anyhow::ensure!(
                    current_balance == expected_balance,
                    "Balance assertion failed"
                );
                return Ok(());
            }
            ExecutableAssertion::StorageEmptinessAssertion { .. } => {
                warn!("We can't handle storage emptiness assertions");
                return Ok(());
            }
        };

        let transaction_hash = maybe_receipt
            .as_ref()
            .map(|receipt| receipt.transaction_hash);
        let mut resolution_context = ResolutionContext {
            pinned_block: Some(&block),
            transaction_hash: transaction_hash.as_ref(),
            ..self.default_resolution_context()
        };

        let logs = flatten_call_frame_logs(&trace);

        for assertion in assertions {
            let expected_status = !assertion.exception;
            let actual_status = trace.error.is_none() && trace.revert_reason.is_none();
            ensure!(
                expected_status == actual_status,
                "Transaction status assertion failed. Expected = {}, actual = {}",
                expected_status,
                actual_status
            );

            if let Some(expected_output) = assertion.return_data {
                let actual_output = trace.output.clone().unwrap_or_default();
                ensure!(
                    expected_output
                        .is_equivalent(&actual_output, &mut resolution_context)
                        .await
                        .context("Equivalency check failed in assertion")?,
                    "Output assertion failed. Expected = {:?}, actual = {}",
                    expected_output,
                    actual_output
                )
            }

            if let Some(expected_events) = assertion.events {
                ensure!(
                    expected_events.len() == logs.len(),
                    "Expected events length are not equal to actual events. Expected = {}, actual = {}",
                    expected_events.len(),
                    logs.len()
                );

                for (i, (expected, actual)) in
                    expected_events.into_iter().zip(logs.iter()).enumerate()
                {
                    if let Some(expected_emitter) = expected.address {
                        let expected_emitter = expected_emitter
                            .resolve_address(&mut resolution_context)
                            .await
                            .context("Failed to resolve event emitter address")?;
                        let actual_emitter = actual.address;
                        ensure!(
                            expected_emitter == actual_emitter,
                            "Event emitter is not the same for event index {}. Expected = {}, actual = {}",
                            i,
                            expected_emitter,
                            actual_emitter
                        );
                    }

                    let expected_number_of_topics = expected.topics.len();
                    let actual_number_of_topics = expected.topics.len();
                    ensure!(
                        expected_number_of_topics == actual_number_of_topics,
                        "Number of topics is not the same for event index {}. Expected = {}, actual = {}",
                        i,
                        expected_number_of_topics,
                        actual_number_of_topics
                    );

                    for (expected_topic, actual_topic) in
                        expected.topics.into_iter().zip(actual.topics())
                    {
                        ensure!(
                            Calldata::new_compound([expected_topic.clone()])
                                .is_equivalent(actual_topic.as_slice(), &mut resolution_context)
                                .await
                                .context("Equivalency check failed in assertion")?,
                            "Event topic is not the same for event index {} assertion failed. Expected = {}, actual = {}",
                            i,
                            expected_topic,
                            actual_topic
                        )
                    }

                    let expected_event_value = expected.values;
                    let actual_event_value = actual.data.data.to_vec();
                    ensure!(
                        expected_event_value
                            .is_equivalent(actual_event_value.as_slice(), &mut resolution_context)
                            .await
                            .context("Equivalency check failed in assertion")?,
                        "Event value is not the same for event index {} assertion failed. Expected = {:?}, actual = {:?}",
                        i,
                        expected_event_value,
                        actual_event_value
                    )
                }
            }
        }

        Ok(())
    }

    async fn contract_instance_of_ref(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<ContractInstance> {
        match contract_ref {
            ContractInstanceOrReference::Reference(value) => {
                let value = value
                    .as_inner()
                    .clone()
                    .resolve(&mut self.default_resolution_context())
                    .await
                    .context("Failed to resolve contract reference")?;
                let CalldataToken::Item(value) = value else {
                    bail!("Resolved reference is not a valid calldata item")
                };
                let index = usize::try_from(value).context("Failed to convert index into value")?;
                self.test_definition
                    .metadata
                    .contracts
                    .as_ref()
                    .and_then(|contracts| contracts.get_index(index).map(|(instance, _)| instance))
                    .context("Failed to resolve reference to instance")
                    .cloned()
            }
            ContractInstanceOrReference::Instance(value) => Ok(value.clone().into_owned()),
        }
    }

    async fn address_of_instance(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<LazyFutureValue<'static, Result<Address>>> {
        let instance = self
            .contract_instance_of_ref(contract_ref)
            .await
            .context("Failed to resolve the ref to instance")?;
        let address = self
            .deployed_contracts
            .get(&instance)
            .map(|value| value.0.clone());
        // Handling of implicit deployment on first access.
        match address {
            Some(address) => Ok(address),
            None => {
                debug!(?contract_ref, "Performing implicit deployment");
                let deployer = self.test_definition.case.deployer_address();
                let (bytecode, _) = self
                    .compilation_artifacts_of_instance(contract_ref)
                    .await
                    .context("No compilation artifacts found")?;
                let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                    TransactionRequest::default().from(deployer),
                    bytecode.as_ref().to_vec(),
                );
                let step_state = self.step_state.take();
                let (tx_hash, receipt) = self
                    .execute_transaction(tx)
                    .await
                    .context("Failed to execute deploy transaction")?;
                self.step_state = step_state;
                let contract_address = receipt.contract_address();
                self.execution_side_effect(ExecutionSideEffect::AssignDeployedContract {
                    instance,
                    address: contract_address.clone(),
                    transaction: tx_hash,
                })
                .await
                .context("Failed to persist the address to state")?;
                Ok(contract_address)
            }
        }
    }

    async fn compilation_artifacts_of_instance(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<(Arc<Vec<u8>>, Arc<JsonAbi>)> {
        let contracts = self
            .test_definition
            .metadata
            .contract_sources()
            .context("No contracts in the metadata")?;
        let instance = self
            .contract_instance_of_ref(contract_ref)
            .await
            .context("Failed to get instance from ref")?;
        let instance_information = contracts
            .get(&instance)
            .context("No information for contract instance in metadata")?;
        self.compiled_contracts
            .get(&instance_information.contract_source_path)
            .and_then(|values| values.get(instance_information.contract_ident.as_inner().as_str()))
            .context("Could not find the contract information")
            .map(|value| (value.0.clone(), value.1.clone()))
    }

    fn trace_transaction(
        &mut self,
        tx_hash: TxHash,
    ) -> LazyFutureValue<'static, Result<CallFrame>> {
        let connector = self.connector.clone();
        LazyFutureValue::new(move || {
            let connector = connector.clone();
            async move {
                let trace = connector
                    .trace_transaction(tx_hash, Self::call_frame_tracing_options())
                    .await;
                trace.map(|trace| {
                    trace
                        .try_into_call_frame()
                        .expect("qed; we requested a call-frame trace")
                })
            }
        })
    }

    fn trace_call(
        &mut self,
        call: TransactionRequest,
    ) -> LazyFutureValue<'static, Result<(BlockPair, CallFrame)>> {
        let connector = self.connector.clone();
        LazyFutureValue::new(move || {
            let call = call.clone();
            let connector = connector.clone();
            async move {
                let current_block = connector.latest_block().await?;
                let trace = connector
                    .trace_call(
                        call,
                        current_block.clone(),
                        Self::call_frame_tracing_options(),
                    )
                    .await
                    .context("Failed to get the trace")?
                    .try_into_call_frame()
                    .expect("qed; we requested a call-frame trace");
                anyhow::Result::<_, anyhow::Error>::Ok((current_block, trace))
            }
        })
    }

    async fn add_forks(
        &mut self,
        count: usize,
        await_receipts: bool,
        consolidate_state: bool,
        capture_index: Option<String>,
        steps: Vec<Step>,
    ) -> Result<()> {
        let Some(ref mut step_state) = self.step_state else {
            panic!("This is a bug, adding a fork with no step state")
        };

        let capture_index = capture_index
            .map(|capture_index| {
                capture_index
                    .strip_prefix("$VARIABLE:")
                    .map(ToOwned::to_owned)
                    .context("Provided capture index is not a valid variable")
            })
            .transpose()?;

        let steps = steps
            .iter()
            .cloned()
            .enumerate()
            .map(|(step_idx, step)| (step_state.step_path.append(StepIdx::new(step_idx)), step))
            .collect::<Vec<_>>();

        let count = self.repetition_count_override.unwrap_or(count);
        let forks = (0..count)
            .map(|i| {
                let fork = Self {
                    id: next_interpreter_id(),
                    platform_information: self.platform_information,
                    test_definition: self.test_definition,
                    connector: self.connector.clone(),
                    private_key_allocator: self.private_key_allocator.clone(),
                    watcher_tx: self.watcher_tx.clone(),
                    repetition_count_override: self.repetition_count_override,
                    step_state: None,
                    variables: self.variables.clone(),
                    deployed_contracts: self.deployed_contracts.clone(),
                    compiled_contracts: self.compiled_contracts.clone(),
                    tasks: vec![],
                    await_receipts,
                    run_assertions: self.run_assertions,
                    steps: steps.clone().into_iter(),
                    gas_cache: self.gas_cache.clone(),
                    run_reporter: self.run_reporter,
                };
                (i, fork)
            })
            .map(|(i, mut fork)| {
                let capture_index = capture_index.clone();
                async move {
                    if let Some(capture_index) = capture_index {
                        let index = U256::try_from(i).expect("qed; u64 -> U256");
                        fork.execution_side_effect(ExecutionSideEffect::AssignVariable {
                            key: capture_index.to_string(),
                            value: LazyFutureValue::new(move || ready(Ok(index))),
                        })
                        .await?
                    };
                    Ok::<_, anyhow::Error>(fork)
                }
            });

        step_state.forked_interpreters = try_join_all(forks)
            .await
            .context("Failed to create interpreter forks")?;
        step_state.merge_forked_interpreters = consolidate_state;

        Ok(())
    }

    async fn execution_side_effect(
        &mut self,
        side_effect: impl Into<ExecutionSideEffect>,
    ) -> Result<()> {
        match side_effect.into() {
            ExecutionSideEffect::AssignVariable { key, value } => {
                self.variables.insert(key.clone(), value.clone());
                self.tasks.push(tokio::spawn(async move {
                    debug!(key, "Awaiting variable resolution");
                    let value = *value
                        .get()
                        .await
                        .as_ref()
                        .expect("fatal: failed to get variable");
                    debug!(
                        key,
                        value = ?value,
                        "Assigned variable"
                    );
                }));
            }
            ExecutionSideEffect::AssignDeployedContract {
                instance,
                address,
                transaction,
            } => {
                let (_, abi) = self
                    .compilation_artifacts_of_instance(&ContractInstanceOrReference::Instance(
                        Cow::Borrowed(&instance),
                    ))
                    .await
                    .context("Failed to get contract information")?;
                let connector = self.connector.clone();
                let await_receipt = self.await_receipts;
                let addr = LazyFutureValue::new(move || {
                    let connector = connector.clone();
                    let address = address.clone();
                    async move {
                        if await_receipt {
                            address.get_owned_error().await.copied()
                        } else {
                            // Optimization: if we're not awaiting receipts then do not wait for the
                            // receipt just to get the contract address from a create transaction.
                            // Instead, derive it from the account address and the nonce and make it
                            // available for steps which need it.
                            let tx = connector.get_transaction(transaction)?;
                            let from = tx
                                .recover_signer()
                                .expect("qed; we signed this, it can't fail recovery");
                            let nonce = match tx {
                                alloy::consensus::EthereumTxEnvelope::Legacy(signed) => {
                                    signed.tx().nonce
                                }
                                alloy::consensus::EthereumTxEnvelope::Eip2930(signed) => {
                                    signed.tx().nonce
                                }
                                alloy::consensus::EthereumTxEnvelope::Eip1559(signed) => {
                                    signed.tx().nonce
                                }
                                alloy::consensus::EthereumTxEnvelope::Eip7702(signed) => {
                                    signed.tx().nonce
                                }
                                alloy::consensus::EthereumTxEnvelope::Eip4844(signed) => {
                                    signed.tx().nonce()
                                }
                            };
                            Ok(from.create(nonce))
                        }
                    }
                });
                self.deployed_contracts
                    .insert(instance.clone(), (addr.clone(), abi));

                let reporter = self
                    .run_reporter
                    .then_some(self.platform_information.reporter.clone());
                self.tasks.push(tokio::spawn(async move {
                    debug!(?instance, "Awaiting address resolution");
                    let value = *addr
                        .get()
                        .await
                        .as_ref()
                        .expect("fatal: failed to get address for contract");
                    debug!(?instance, address = ?value, "Assigned instance address");
                    if let Some(reporter) = reporter {
                        reporter
                            .report_contract_deployed_event(instance.clone(), value)
                            .expect("fatal: reporter is configured but we can't send to it");
                    }
                }));
            }
        }
        Ok(())
    }
}

impl<'a> LazyResolverApi for Interpreter<'a> {
    async fn get_contract_address(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> anyhow::Result<Address> {
        let fut = Box::pin(self.address_of_instance(contract_ref));
        let address = fut.await.context("Failed to get the instance address")?;
        address.get_owned_error().await.copied()
    }

    async fn get_variable(&mut self, variable: impl AsRef<str>) -> Option<anyhow::Result<U256>> {
        trace!(
            requested = variable.as_ref(),
            available = ?self.variables.keys().collect::<Vec<_>>(),
            "Requested a variable"
        );
        let variable = self.variables.get(variable.as_ref())?;
        Some(variable.get_owned_error().await.copied())
    }
}

trait InterpreterApi {
    async fn execute_transaction(
        &mut self,
        tx: TransactionRequest,
    ) -> Result<(
        TxHash,
        LazyFutureValue<'static, Result<Arc<TransactionReceipt>>>,
    )>;

    async fn allocate_account(&mut self) -> LazyFutureValue<'static, Result<Address>>;

    async fn resolve_variable_name(&mut self, raw_name: &str) -> Result<String>;

    async fn run_assertion(&mut self, assertion: ExecutableAssertion) -> Result<()>;

    async fn contract_instance_of_ref(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<ContractInstance>;

    async fn address_of_instance(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<LazyFutureValue<'static, Result<Address>>>;

    async fn compilation_artifacts_of_instance(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<(Arc<Vec<u8>>, Arc<JsonAbi>)>;

    fn trace_transaction(&mut self, tx_hash: TxHash)
    -> LazyFutureValue<'static, Result<CallFrame>>;

    fn trace_call(
        &mut self,
        call: TransactionRequest,
    ) -> LazyFutureValue<'static, Result<(BlockPair, CallFrame)>>;

    async fn add_forks(
        &mut self,
        count: usize,
        await_receipts: bool,
        consolidate_state: bool,
        capture_index: Option<String>,
        steps: Vec<Step>,
    ) -> Result<()>;

    async fn execution_side_effect(
        &mut self,
        side_effect: impl Into<ExecutionSideEffect>,
    ) -> Result<()>;
}

trait ExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput>;
}

#[derive(Default)]
struct ExecutionOutput {
    next_steps: Vec<AnyExecutableStep>,
}

impl ExecutionOutput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_next_step(mut self, next_step: impl Into<AnyExecutableStep>) -> Self {
        self.next_steps.push(next_step.into());
        self
    }
}

enum ExecutionSideEffect {
    AssignVariable {
        key: String,
        value: LazyFutureValue<'static, Result<U256>>,
    },
    AssignDeployedContract {
        instance: ContractInstance,
        address: LazyFutureValue<'static, Result<Address>>,
        transaction: TxHash,
    },
}

#[derive(derive_more::From)]
enum AnyExecutableStep {
    Repetition(RepetitionExecutableStep),
    AllocateAccount(AllocateAccountExecutableStep),
    Transfer(TransferExecutableStep),
    StorageContentAssertion(StorageContentAssertionExecutableStep),
    BalanceAssertion(BalanceAssertionExecutableStep),
    Transaction(TransactionExecutableStep),
    ExpectedSuccessTransaction(ExpectedSuccessTransactionExecutableStep),
    ExpectedFailureTransaction(ExpectedFailureTransactionExecutableStep),
}

impl ExecutableStep for AnyExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        match self {
            Self::Repetition(step) => step.execute(api).await,
            Self::AllocateAccount(step) => step.execute(api).await,
            Self::Transfer(step) => step.execute(api).await,
            Self::StorageContentAssertion(step) => step.execute(api).await,
            Self::BalanceAssertion(step) => step.execute(api).await,
            Self::Transaction(step) => step.execute(api).await,
            Self::ExpectedSuccessTransaction(step) => step.execute(api).await,
            Self::ExpectedFailureTransaction(step) => step.execute(api).await,
        }
    }
}

struct StorageContentAssertionExecutableStep {
    address: Address,
    is_storage_empty: bool,
}

impl ExecutableStep for StorageContentAssertionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        api.run_assertion(ExecutableAssertion::StorageEmptinessAssertion {
            _address: self.address,
            _is_empty: self.is_storage_empty,
        })
        .await
        .context("Failed to run storage empty assertion")?;
        Ok(Default::default())
    }
}

struct BalanceAssertionExecutableStep {
    address: Address,
    balance: U256,
}

impl ExecutableStep for BalanceAssertionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        api.run_assertion(ExecutableAssertion::BalanceAssertion {
            address: self.address,
            balance: self.balance,
        })
        .await
        .context("Failed to run balance assertion")?;
        Ok(Default::default())
    }
}

struct AllocateAccountExecutableStep {
    variable_name: String,
}

impl ExecutableStep for AllocateAccountExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        let variable_name = self
            .variable_name
            .strip_prefix("$VARIABLE:")
            .context("Variables must start with $VARIABLE")?;
        let variable_name = api
            .resolve_variable_name(variable_name)
            .await
            .context("Failed to resolve allocate_account variable name")?;
        let account = api.allocate_account().await.map(|account| {
            account
                .as_ref()
                .map(|account| U256::from_be_slice(account.as_slice()))
                .map_err(|err| anyhow!("Failed to allocate a new account: {err}"))
        });
        api.execution_side_effect(ExecutionSideEffect::AssignVariable {
            key: variable_name,
            value: account,
        })
        .await
        .context("Interpreter failed to handle the side-effect")?;
        Ok(Default::default())
    }
}

struct TransferExecutableStep {
    from: Address,
    to: Address,
    amount: U256,
}

impl ExecutableStep for TransferExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        let tx = TransactionRequest::default()
            .from(self.from)
            .to(self.to)
            .value(self.amount);
        api.execute_transaction(tx)
            .await
            .context("Transfer failed")?;
        Ok(ExecutionOutput::new())
    }
}

struct TransactionExecutableStep {
    from: Address,
    to: ContractInstanceOrReference<'static>,
    value: U256,
    method: Method,
    calldata: Bytes,
    expectations: Vec<ExpectedOutput>,
    variable_assignment: Option<VariableAssignments>,
    gas_override: Option<GasOverrides>,
}

impl ExecutableStep for TransactionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        let Self {
            from,
            to,
            value,
            method,
            calldata,
            expectations,
            variable_assignment,
            gas_override,
        } = self;
        info!(?from, ?to, ?value, ?method, "Transaction execution step");

        let is_expected_to_fail = expectations.iter().any(|assertion| assertion.exception);
        let mut deploys = None;

        let mut tx = TransactionRequest::default().from(from).value(value);
        if let Some(ref gas_override) = gas_override {
            gas_override.apply_to::<Ethereum>(&mut tx);
        }
        tx = match method {
            Method::Deployer => {
                let (bytecode, _) = api
                    .compilation_artifacts_of_instance(&to)
                    .await
                    .context("Failed to get compilation artifacts")?;
                let instance = api
                    .contract_instance_of_ref(&to)
                    .await
                    .expect("qed; we were just able to get information for it");
                deploys = Some(instance);
                let mut bytecode = bytecode.as_ref().clone();
                bytecode.extend(calldata);
                TransactionBuilder::<Ethereum>::with_deploy_code(tx, bytecode)
            }
            Method::Fallback => {
                let address = api.address_of_instance(&to).await?;
                let address = *address
                    .get_owned_error()
                    .await
                    .context("Failed to get address of instance")?;
                if !calldata.is_empty() {
                    tx.to(address).input(calldata.into())
                } else {
                    tx.to(address)
                }
            }
            Method::FunctionName(func_signature) => {
                let address = api.address_of_instance(&to).await?;
                let address = *address
                    .get_owned_error()
                    .await
                    .context("Failed to get address of instance")?;

                let selector = if func_signature.contains("(") && func_signature.contains(")") {
                    Function::parse(&func_signature)
                        .context("Invalid function signature")?
                        .selector()
                } else {
                    let (_, abi) = api.compilation_artifacts_of_instance(&to).await.expect(
                        "qed; this is a deployed contract and we just obtained its address",
                    );
                    abi.functions()
                        .find(|function| function.signature().starts_with(func_signature.as_str()))
                        .context("No function with the required name in the ABI")?
                        .selector()
                };

                let mut new_calldata = Vec::with_capacity(4 + calldata.len());
                new_calldata.extend(selector);
                new_calldata.extend(calldata.to_vec());

                tx.to(address).input(new_calldata.into())
            }
        };

        let output = if is_expected_to_fail {
            ExecutionOutput::new()
                .with_next_step(ExpectedFailureTransactionExecutableStep { tx, expectations })
        } else {
            ExecutionOutput::new().with_next_step(ExpectedSuccessTransactionExecutableStep {
                tx,
                expectations,
                deploys,
                variable_assignment,
            })
        };
        Ok(output)
    }
}

struct ExpectedSuccessTransactionExecutableStep {
    tx: TransactionRequest,
    expectations: Vec<ExpectedOutput>,
    deploys: Option<ContractInstance>,
    variable_assignment: Option<VariableAssignments>,
}

impl ExecutableStep for ExpectedSuccessTransactionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        let (tx_hash, receipt) = api
            .execute_transaction(self.tx.clone())
            .await
            .context("Failed to submit transaction")?;

        if let Some(deploys_contract_instance) = self.deploys {
            let contract_address = receipt.contract_address();
            api.execution_side_effect(ExecutionSideEffect::AssignDeployedContract {
                instance: deploys_contract_instance,
                address: contract_address,
                transaction: tx_hash,
            })
            .await
            .context("Interpreter failed to handle side-effect")?;
        }

        let trace = api.trace_transaction(tx_hash);
        if let Some(assignment) = self.variable_assignment {
            for (index, raw_name) in assignment.names().iter().enumerate() {
                let key = api
                    .resolve_variable_name(raw_name)
                    .await
                    .context("Failed to resolve variable assignment name")?;

                let value = match &assignment {
                    VariableAssignments::ReturnData { .. } => {
                        let start = index * 32;
                        let end = start + 32;
                        trace.map(move |trace| {
                            trace
                                .as_ref()
                                .map_err(|err| anyhow!("Failed to get trace: {err}"))
                                .and_then(|trace| {
                                    trace
                                        .output
                                        .clone()
                                        .unwrap_or_default()
                                        .get(start..end)
                                        .map(U256::from_be_slice)
                                        .context(
                                            "Requested variable assignment isn't within range \
                                             of output",
                                        )
                                })
                        })
                    }
                    VariableAssignments::EventTopics { topics, .. } => {
                        let topic = topics.get(index).with_context(|| {
                            format!("No event topic provided for variable at index {index}")
                        })?;
                        let log_index = topic.log_index;
                        let topic_index = topic.topic_index;
                        receipt.map(move |receipt| {
                            receipt
                                .as_ref()
                                .map_err(|err| anyhow!("Failed to get receipt: {err}"))
                                .and_then(|receipt| {
                                    let log = receipt.logs().get(log_index).with_context(|| {
                                        format!("Log index {log_index} out of range in receipt")
                                    })?;
                                    let topic_value =
                                        log.topics().get(topic_index).with_context(|| {
                                            format!(
                                                "Topic index {topic_index} out of range in log \
                                                 {log_index}"
                                            )
                                        })?;
                                    Ok(U256::from_be_slice(topic_value.as_slice()))
                                })
                        })
                    }
                };

                api.execution_side_effect(ExecutionSideEffect::AssignVariable { key, value })
                    .await
                    .context("Interpreter failed to handle side-effect")?;
            }
        }

        api.run_assertion(
            ExecutableAssertion::TransactionAssertionsForExpectedSuccess {
                assertions: self.expectations,
                receipt,
                trace,
            },
        )
        .await
        .context("Assertions failed for transaction")?;

        Ok(Default::default())
    }
}

struct ExpectedFailureTransactionExecutableStep {
    tx: TransactionRequest,
    expectations: Vec<ExpectedOutput>,
}

impl ExecutableStep for ExpectedFailureTransactionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        let trace = api.trace_call(self.tx);

        api.run_assertion(
            ExecutableAssertion::TransactionAssertionsForExpectedFailure {
                assertions: self.expectations,
                trace,
            },
        )
        .await
        .context("Assertions failed for transaction")?;

        Ok(Default::default())
    }
}

enum ExecutableAssertion {
    TransactionAssertionsForExpectedSuccess {
        assertions: Vec<ExpectedOutput>,
        receipt: LazyFutureValue<'static, Result<Arc<TransactionReceipt>>>,
        trace: LazyFutureValue<'static, Result<CallFrame>>,
    },
    TransactionAssertionsForExpectedFailure {
        assertions: Vec<ExpectedOutput>,
        trace: LazyFutureValue<'static, Result<(BlockPair, CallFrame)>>,
    },
    BalanceAssertion {
        address: Address,
        balance: U256,
    },
    StorageEmptinessAssertion {
        _address: Address,
        _is_empty: bool,
    },
}

struct RepetitionExecutableStep {
    repetition_count: usize,
    await_receipts: bool,
    consolidate_state: bool,
    capture_index: Option<String>,
    steps: Vec<Step>,
}

impl ExecutableStep for RepetitionExecutableStep {
    async fn execute(self, api: &mut impl InterpreterApi) -> Result<ExecutionOutput> {
        api.add_forks(
            self.repetition_count,
            self.await_receipts,
            self.consolidate_state,
            self.capture_index,
            self.steps,
        )
        .await
        .context("Failed to add interpreter forks")?;
        Ok(Default::default())
    }
}

// region:Helpers
fn flatten_call_frame_logs(frame: &CallFrame) -> Vec<Log> {
    if frame.error.is_some() || frame.revert_reason.is_some() {
        return vec![];
    }
    let mut logs = Vec::new();
    let mut next_child = 0;
    for log in frame.logs.iter() {
        let position = log.position.unwrap_or_default() as usize;
        while next_child < frame.calls.len() && next_child < position {
            logs.extend(flatten_call_frame_logs(&frame.calls[next_child]));
            next_child += 1;
        }
        logs.push(Log::new_unchecked(
            log.address.unwrap_or_default(),
            log.topics.clone().unwrap_or_default(),
            log.data.clone().unwrap_or_default(),
        ));
    }
    for child in frame.calls.iter().skip(next_child) {
        logs.extend(flatten_call_frame_logs(child));
    }
    logs
}
// endregion:Helpers
