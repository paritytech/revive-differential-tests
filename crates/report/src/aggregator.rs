//! Implementation of the report aggregator task which consumes the events sent by the various
//! reporters and combines them into a single unified report.

use crate::internal_prelude::*;

pub struct ReportAggregator {
    /* Internal Report State */
    report: Report,
    remaining_cases: HashMap<MetadataFilePath, HashMap<Mode, HashSet<CaseIdx>>>,
    remaining_compilation_modes: HashMap<MetadataFilePath, HashSet<Mode>>,
    /* Channels */
    runner_tx: Option<UnboundedSender<RunnerEvent>>,
    runner_rx: UnboundedReceiver<RunnerEvent>,
    listener_tx: Sender<ReporterEvent>,
    /* Context */
    file_name: Option<String>,
}

impl ReportAggregator {
    pub fn new(context: Context) -> Self {
        let (runner_tx, runner_rx) = unbounded_channel::<RunnerEvent>();
        let (listener_tx, _) = channel::<ReporterEvent>(0xFFFF);
        Self {
            file_name: match context {
                Context::Test(ref context) => context.report.file_name.clone(),
                Context::Benchmark(ref context) => context.report.file_name.clone(),
                Context::Compile(ref context) => context.report.file_name.clone(),
                Context::ExportJsonSchema(_)
                | Context::ExportGenesis(..)
                | Context::ExportTestSpecifiers(..) => None,
            },
            report: Report::new(context),
            remaining_cases: Default::default(),
            remaining_compilation_modes: Default::default(),
            runner_tx: Some(runner_tx),
            runner_rx,
            listener_tx,
        }
    }

    pub fn into_task(mut self) -> (Reporter, FrameworkFuture<Result<Report>>) {
        let reporter = self
            .runner_tx
            .take()
            .map(Into::into)
            .expect("Can't fail since this can only be called once");
        (reporter, self.aggregate())
    }

    fn aggregate(mut self) -> FrameworkFuture<Result<Report>> {
        Box::pin(async move {
            debug!("Starting to aggregate report");

            while let Some(event) = self.runner_rx.recv().await {
                debug!(event = event.variant_name(), "Received Event");
                match event {
                    RunnerEvent::SubscribeToEvents(event) => {
                        self.handle_subscribe_to_events_event(*event);
                    }
                    RunnerEvent::MetadataFileDiscovery(event) => {
                        self.handle_metadata_file_discovery_event(*event);
                    }
                    RunnerEvent::TestCaseDiscovery(event) => {
                        self.handle_test_case_discovery(*event);
                    }
                    RunnerEvent::PostLinkCompilationDiscovery(event) => {
                        self.handle_post_link_compilation_discovery(*event);
                    }
                    RunnerEvent::TestSucceeded(event) => {
                        self.handle_test_succeeded_event(*event);
                    }
                    RunnerEvent::TestFailed(event) => {
                        self.handle_test_failed_event(*event);
                    }
                    RunnerEvent::TestIgnored(event) => {
                        self.handle_test_ignored_event(*event);
                    }
                    RunnerEvent::NodeAssigned(event) => {
                        self.handle_node_assigned_event(*event);
                    }
                    RunnerEvent::PreLinkContractsCompilationSucceeded(event) => {
                        self.handle_pre_link_contracts_compilation_succeeded_event(*event)
                    }
                    RunnerEvent::PostLinkContractsCompilationSucceeded(event) => {
                        self.handle_post_link_contracts_compilation_succeeded_event(*event)
                    }
                    RunnerEvent::PreLinkContractsCompilationFailed(event) => {
                        self.handle_pre_link_contracts_compilation_failed_event(*event)
                    }
                    RunnerEvent::PostLinkContractsCompilationFailed(event) => {
                        self.handle_post_link_contracts_compilation_failed_event(*event)
                    }
                    RunnerEvent::PostLinkContractsCompilationIgnored(event) => {
                        self.handle_post_link_contracts_compilation_ignored_event(*event);
                    }
                    RunnerEvent::LibrariesDeployed(event) => {
                        self.handle_libraries_deployed_event(*event);
                    }
                    RunnerEvent::ContractDeployed(event) => {
                        self.handle_contract_deployed_event(*event);
                    }
                    RunnerEvent::Completion(_) => {
                        break;
                    }
                    /* Benchmarks Events */
                    RunnerEvent::StepTransactionInformation(event) => {
                        self.handle_step_transaction_information(*event)
                    }
                    RunnerEvent::ContractInformation(event) => {
                        self.handle_contract_information(*event);
                    }
                    RunnerEvent::BlockMined(event) => self.handle_block_mined(*event),
                }
            }
            self.handle_completion(CompletionEvent {});
            debug!("Report aggregation completed");

            let default_file_name = {
                let current_timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("System clock is before UNIX_EPOCH; cannot compute report timestamp")?
                    .as_secs();
                let mut file_name = current_timestamp.to_string();
                file_name.push_str(".json");
                file_name
            };
            let file_name = self.file_name.unwrap_or(default_file_name);
            let file_path = self
                .report
                .context
                .as_working_directory_configuration()
                .working_directory
                .as_path()
                .join(file_name);
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .read(false)
                .open(&file_path)
                .with_context(|| {
                    format!(
                        "Failed to open report file for writing: {}",
                        file_path.display()
                    )
                })?;
            serde_json::to_writer_pretty(&file, &self.report).with_context(|| {
                format!("Failed to serialize report JSON to {}", file_path.display())
            })?;
            tracing::info!(file_path = %file_path.display(), "Report has been written");
            Ok(self.report)
        })
    }

    fn handle_subscribe_to_events_event(&self, event: SubscribeToEventsEvent) {
        let _ = event.tx.send(self.listener_tx.subscribe());
    }

    fn handle_metadata_file_discovery_event(&mut self, event: MetadataFileDiscoveryEvent) {
        self.report.metadata_files.insert(event.path.clone());
    }

    fn handle_test_case_discovery(&mut self, event: TestCaseDiscoveryEvent) {
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.compiler_mode.clone())
            .or_default()
            .insert(event.test_specifier.case_idx);
    }

    fn handle_post_link_compilation_discovery(&mut self, event: PostLinkCompilationDiscoveryEvent) {
        self.remaining_compilation_modes
            .entry(
                event
                    .post_link_compilation_specifier
                    .metadata_file_path
                    .clone()
                    .into(),
            )
            .or_default()
            .insert(event.post_link_compilation_specifier.compiler_mode.clone());
    }

    fn handle_test_succeeded_event(&mut self, event: TestSucceededEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remove_remaining_case(&event.test_specifier);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Succeeded {
            steps_executed: event.steps_executed,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_test_failed_event(&mut self, event: TestFailedEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remove_remaining_case(&event.test_specifier);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Failed {
            reason: event.reason,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_test_ignored_event(&mut self, event: TestIgnoredEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remove_remaining_case(&event.test_specifier);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Ignored {
            reason: event.reason,
            additional_fields: event.additional_fields,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_post_test_case_status_update(&mut self, specifier: &TestSpecifier) {
        let remaining_cases = self
            .remaining_cases
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(specifier.compiler_mode.clone())
            .or_default();
        if !remaining_cases.is_empty() {
            return;
        }

        let case_status = self
            .report
            .execution_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .case_reports
            .iter()
            .flat_map(|(case_idx, mode_to_execution_map)| {
                let case_status = mode_to_execution_map
                    .mode_execution_reports
                    .get(&specifier.compiler_mode)?
                    .status
                    .clone()
                    .expect("Can't be uninitialized");
                Some((*case_idx, case_status))
            })
            .collect::<BTreeMap<_, _>>();
        let event = ReporterEvent::MetadataFileSolcModeCombinationExecutionCompleted {
            metadata_file_path: specifier.metadata_file_path.clone().into(),
            mode: specifier.compiler_mode.clone(),
            case_status,
        };

        // According to the documentation on send, the sending fails if there are no more receiver
        // handles. Therefore, this isn't an error that we want to bubble up or anything. If we fail
        // to send then we ignore the error.
        let _ = self.listener_tx.send(event);
    }

    fn handle_node_assigned_event(&mut self, event: NodeAssignedEvent) {
        let execution_information = self.execution_information(&ExecutionSpecifier {
            test_specifier: event.test_specifier,
            node_id: event.id,
            platform_identifier: event.platform_identifier,
        });
        execution_information.node = Some(TestCaseNodeInformation {
            id: event.id,
            platform_identifier: event.platform_identifier,
            connection_string: event.connection_string,
        });
    }

    fn handle_pre_link_contracts_compilation_succeeded_event(
        &mut self,
        event: PreLinkContractsCompilationSucceededEvent,
    ) {
        let report_configuration = self.report.context.as_report_configuration();
        let compiler_input = if report_configuration.include_compiler_input {
            event.compiler_input
        } else {
            None
        };

        let status = CompilationStatus::Success {
            is_cached: event.is_cached,
            compiler_version: event.compiler_version,
            compiler_path: event.compiler_path,
            compiler_input,
            compiled_contracts_info: Self::generate_compiled_contracts_info(
                event.compiler_output,
                report_configuration.include_compiler_output,
            ),
        };

        let execution_information = self.execution_information(&event.execution_specifier);
        execution_information.pre_link_compilation_status = Some(status);
    }

    fn handle_post_link_contracts_compilation_succeeded_event(
        &mut self,
        event: PostLinkContractsCompilationSucceededEvent,
    ) {
        let report_configuration = self.report.context.as_report_configuration();
        let compiler_input = if report_configuration.include_compiler_input {
            event.compiler_input
        } else {
            None
        };

        let status = CompilationStatus::Success {
            is_cached: event.is_cached,
            compiler_version: event.compiler_version,
            compiler_path: event.compiler_path,
            compiler_input,
            compiled_contracts_info: Self::generate_compiled_contracts_info(
                event.compiler_output,
                report_configuration.include_compiler_output,
            ),
        };

        match &event.specifier {
            CompilationSpecifier::Execution(specifier) => {
                let execution_information = self.execution_information(specifier);
                execution_information.post_link_compilation_status = Some(status);
            }
            CompilationSpecifier::PostLink(specifier) => {
                let report = self.post_link_compilation_report(specifier);
                report.status = Some(status);
                self.handle_post_post_link_contracts_compilation_status_update(specifier);
            }
        }
    }

    fn handle_pre_link_contracts_compilation_failed_event(
        &mut self,
        event: PreLinkContractsCompilationFailedEvent,
    ) {
        let status = CompilationStatus::Failure {
            reason: event.reason,
            compiler_version: event.compiler_version,
            compiler_path: event.compiler_path,
            compiler_input: event.compiler_input,
        };

        let execution_information = self.execution_information(&event.execution_specifier);
        execution_information.pre_link_compilation_status = Some(status);
    }

    fn handle_post_link_contracts_compilation_failed_event(
        &mut self,
        event: PostLinkContractsCompilationFailedEvent,
    ) {
        let status = CompilationStatus::Failure {
            reason: event.reason,
            compiler_version: event.compiler_version,
            compiler_path: event.compiler_path,
            compiler_input: event.compiler_input,
        };

        match &event.specifier {
            CompilationSpecifier::Execution(specifier) => {
                let execution_information = self.execution_information(specifier);
                execution_information.post_link_compilation_status = Some(status);
            }
            CompilationSpecifier::PostLink(specifier) => {
                let report = self.post_link_compilation_report(specifier);
                report.status = Some(status);
                self.handle_post_post_link_contracts_compilation_status_update(specifier);
            }
        }
    }

    fn handle_post_link_contracts_compilation_ignored_event(
        &mut self,
        event: PostLinkContractsCompilationIgnoredEvent,
    ) {
        let status = CompilationStatus::Ignored {
            reason: event.reason,
            additional_fields: event.additional_fields,
        };

        let report = self.post_link_compilation_report(&event.post_link_compilation_specifier);
        report.status = Some(status.clone());
        self.handle_post_post_link_contracts_compilation_status_update(
            &event.post_link_compilation_specifier,
        );
    }

    fn handle_post_post_link_contracts_compilation_status_update(
        &mut self,
        specifier: &PostLinkCompilationSpecifier,
    ) {
        // Remove this from the set we're tracking since it has completed.
        self.remove_remaining_compilation_mode(specifier);

        let remaining_modes = self
            .remaining_compilation_modes
            .entry(specifier.metadata_file_path.clone().into())
            .or_default();
        if !remaining_modes.is_empty() {
            return;
        }

        let status_per_mode = self
            .report
            .execution_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .compilation_reports
            .iter()
            .flat_map(|(mode, report)| {
                let status = report.status.clone().expect("Can't be uninitialized");
                Some((mode.clone(), status))
            })
            .collect::<BTreeMap<_, _>>();

        let event = ReporterEvent::MetadataFileModeCombinationCompilationCompleted {
            metadata_file_path: specifier.metadata_file_path.clone().into(),
            compilation_status: status_per_mode,
        };

        // According to the documentation on send, the sending fails if there are no more receiver
        // handles. Therefore, this isn't an error that we want to bubble up or anything. If we fail
        // to send then we ignore the error.
        let _ = self.listener_tx.send(event);
    }

    fn handle_libraries_deployed_event(&mut self, event: LibrariesDeployedEvent) {
        let execution_information = self.execution_information(&event.execution_specifier);
        let deployed_libraries = execution_information
            .deployed_libraries
            .get_or_insert_default();
        for (contract_instance, address) in event.libraries {
            deployed_libraries.insert(contract_instance, address);
        }
    }

    fn handle_contract_deployed_event(&mut self, event: ContractDeployedEvent) {
        self.execution_information(&event.execution_specifier)
            .deployed_contracts
            .get_or_insert_default()
            .insert(event.contract_instance.clone(), event.address);
        self.test_case_report(&event.execution_specifier.test_specifier)
            .contract_addresses
            .entry(event.contract_instance)
            .or_default()
            .entry(event.execution_specifier.platform_identifier)
            .or_default()
            .push(event.address);
    }

    fn handle_completion(&mut self, _: CompletionEvent) {
        self.runner_rx.close();
        self.handle_metrics_computation();
    }

    fn handle_metrics_computation(&mut self) {
        for report in self.report.execution_information.values_mut() {
            for report in report.case_reports.values_mut() {
                for report in report.mode_execution_reports.values_mut() {
                    for (platform_identifier, block_information) in
                        report.mined_block_information.iter_mut()
                    {
                        block_information.sort_by(|a, b| {
                            a.ethereum_block_information
                                .block_number
                                .cmp(&b.ethereum_block_information.block_number)
                        });

                        let metrics = compute_metrics_information(block_information);
                        if !metrics.is_empty() {
                            report
                                .metrics_information
                                .insert(*platform_identifier, metrics);
                        }
                    }
                }
            }
        }
    }

    fn handle_step_transaction_information(&mut self, event: StepTransactionInformationEvent) {
        self.test_case_report(&event.execution_specifier.test_specifier)
            .steps
            .entry(event.step_path)
            .or_default()
            .transactions
            .entry(event.execution_specifier.platform_identifier)
            .or_default()
            .push(event.transaction_information);
    }

    fn handle_contract_information(&mut self, event: ContractInformationEvent) {
        self.test_case_report(&event.execution_specifier.test_specifier)
            .compiled_contracts
            .entry(event.source_code_path)
            .or_default()
            .entry(event.contract_name)
            .or_default()
            .contract_size
            .insert(
                event.execution_specifier.platform_identifier,
                event.contract_size,
            );
    }

    fn handle_block_mined(&mut self, event: BlockMinedEvent) {
        self.test_case_report(&event.execution_specifier.test_specifier)
            .mined_block_information
            .entry(event.execution_specifier.platform_identifier)
            .or_default()
            .push(event.mined_block_information);
    }

    fn test_case_report(&mut self, specifier: &TestSpecifier) -> &mut ExecutionReport {
        self.report
            .execution_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .case_reports
            .entry(specifier.case_idx)
            .or_default()
            .mode_execution_reports
            .entry(specifier.compiler_mode.clone())
            .or_default()
    }

    fn execution_information(
        &mut self,
        specifier: &ExecutionSpecifier,
    ) -> &mut ExecutionInformation {
        let test_case_report = self.test_case_report(&specifier.test_specifier);
        test_case_report
            .platform_execution
            .entry(specifier.platform_identifier)
            .or_default()
            .get_or_insert_default()
    }

    fn post_link_compilation_report(
        &mut self,
        specifier: &PostLinkCompilationSpecifier,
    ) -> &mut PostLinkCompilationReport {
        self.report
            .execution_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .compilation_reports
            .entry(specifier.compiler_mode.clone())
            .or_default()
    }

    /// Generates the compiled contract information for each contract at each path.
    fn generate_compiled_contracts_info(
        compiler_output: CompilerOutput,
        include_compiler_output: bool,
    ) -> HashMap<PathBuf, HashMap<String, CompiledContractInformation>> {
        let mut compiled_contracts_info = HashMap::new();

        for (source_path, contracts) in compiler_output.contracts {
            let mut contracts_info_at_path = HashMap::new();

            for (contract_name, (bytecode, abi)) in contracts {
                let (is_valid_hex, bytecode_hash) = Self::hex_decode_and_hash(&bytecode);
                let requires_linking = !is_valid_hex;
                let info = if include_compiler_output {
                    CompiledContractInformation {
                        abi: Some(abi),
                        bytecode: Some(bytecode),
                        bytecode_hash,
                        requires_linking,
                    }
                } else {
                    CompiledContractInformation {
                        abi: None,
                        bytecode: None,
                        bytecode_hash,
                        requires_linking,
                    }
                };
                contracts_info_at_path.insert(contract_name, info);
            }

            compiled_contracts_info.insert(source_path, contracts_info_at_path);
        }

        compiled_contracts_info
    }

    /// Attempts to hex decode the input before hashing the result. If the input
    /// is prefixed with `0x`, the prefix is stripped before decoding and hashing.
    ///
    /// Returns `(true, hash)` if decoding succeeded, with a hash of the raw bytes.
    /// Returns `(false, hash)` if decoding failed due to invalid hex, with a hash of the string.
    fn hex_decode_and_hash(input: &str) -> (bool, B256) {
        let input = input.strip_prefix("0x").unwrap_or(input);

        match hex::decode(input) {
            Ok(bytes) => (true, B256::from_slice(&Sha256::digest(&bytes))),
            Err(_) => (false, B256::from_slice(&Sha256::digest(input.as_bytes()))),
        }
    }

    /// Removes the case specified by the `specifier` from the tracked remaining cases.
    fn remove_remaining_case(&mut self, specifier: &TestSpecifier) {
        self.remaining_cases
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(specifier.compiler_mode.clone())
            .or_default()
            .remove(&specifier.case_idx);
    }

    /// Removes the compilation mode specified by the `specifier` from the tracked remaining compilation modes.
    fn remove_remaining_compilation_mode(&mut self, specifier: &PostLinkCompilationSpecifier) {
        self.remaining_compilation_modes
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .remove(&specifier.compiler_mode);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    /// The context that the tool was started up with.
    pub context: Context,
    /// The list of metadata files that were found by the tool.
    pub metadata_files: BTreeSet<MetadataFilePath>,
    /// Information relating to each metadata file after executing the tool.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub execution_information: BTreeMap<MetadataFilePath, MetadataFileReport>,
}

impl Report {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            metadata_files: Default::default(),
            execution_information: Default::default(),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct MetadataFileReport {
    /// The report of each case keyed by the case idx.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub case_reports: BTreeMap<CaseIdx, CaseReport>,
    /// The [`CompilationReport`] for each of the [`Mode`]s.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[serde_as(as = "BTreeMap<DisplayFromStr, _>")]
    pub compilation_reports: BTreeMap<Mode, PostLinkCompilationReport>,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CaseReport {
    /// The [`ExecutionReport`] for each one of the [`Mode`]s.
    #[serde_as(as = "HashMap<DisplayFromStr, _>")]
    pub mode_execution_reports: HashMap<Mode, ExecutionReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ExecutionReport {
    /// Information on the status of the test case and whether it succeeded, failed, or was ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TestCaseStatus>,
    /// Per-block metrics information for each platform.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics_information: PlatformKeyedInformation<Vec<MetricsInformation>>,
    /// Information related to the execution on one of the platforms.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platform_execution: PlatformKeyedInformation<Option<ExecutionInformation>>,
    /// Information on the compiled contracts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub compiled_contracts: BTreeMap<PathBuf, BTreeMap<String, ContractInformation>>,
    /// The addresses of the deployed contracts
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contract_addresses: BTreeMap<ContractInstance, PlatformKeyedInformation<Vec<Address>>>,
    /// Information on the mined blocks as part of this execution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mined_block_information: PlatformKeyedInformation<Vec<MinedBlockInformation>>,
    /// Information tracked for each step that was executed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<StepPath, StepReport>,
}

/// Information related to the status of the test. Could be that the test succeeded, failed, or that
/// it was ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum TestCaseStatus {
    /// The test case succeeded.
    Succeeded {
        /// The number of steps of the case that were executed.
        steps_executed: usize,
    },
    /// The test case failed.
    Failed {
        /// The reason for the failure of the test case.
        reason: String,
    },
    /// The test case was ignored. This variant carries information related to why it was ignored.
    Ignored {
        /// The reason behind the test case being ignored.
        reason: String,
        /// Additional fields that describe more information on why the test case is ignored.
        #[serde(flatten)]
        additional_fields: IndexMap<String, serde_json::Value>,
    },
}

/// Information related to the platform node that's being used to execute the step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestCaseNodeInformation {
    /// The ID of the node that this case is being executed on.
    pub id: usize,
    /// The platform of the node.
    pub platform_identifier: PlatformIdentifier,
    /// The connection string of the node.
    pub connection_string: String,
}

/// Execution information tied to the platform.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionInformation {
    /// Information related to the node assigned to this test case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<TestCaseNodeInformation>,
    /// Information on the pre-link compiled contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_link_compilation_status: Option<CompilationStatus>,
    /// Information on the post-link compiled contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_link_compilation_status: Option<CompilationStatus>,
    /// Information on the deployed contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_contracts: Option<BTreeMap<ContractInstance, Address>>,
    /// Information on the deployed libraries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_libraries: Option<BTreeMap<ContractInstance, Address>>,
}

/// The post-link-only compilation report.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PostLinkCompilationReport {
    /// The compilation status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CompilationStatus>,
}

/// Information related to compilation
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum CompilationStatus {
    /// The compilation was successful.
    Success {
        /// A flag with information on whether the compilation artifacts were cached or not.
        is_cached: bool,
        /// The version of the compiler used to compile the contracts.
        compiler_version: Version,
        /// The path of the compiler used to compile the contracts.
        compiler_path: PathBuf,
        /// The input provided to the compiler to compile the contracts. This is only included if
        /// the appropriate flag is set in the CLI context and if the contracts were not cached and
        /// the compiler was invoked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiler_input: Option<CompilerInput>,
        /// The information about each compiled contract at each path.
        compiled_contracts_info: HashMap<PathBuf, HashMap<String, CompiledContractInformation>>,
    },
    /// The compilation failed.
    Failure {
        /// The failure reason.
        reason: String,
        /// The version of the compiler used to compile the contracts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiler_version: Option<Version>,
        /// The path of the compiler used to compile the contracts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiler_path: Option<PathBuf>,
        /// The input provided to the compiler to compile the contracts. This is only included if
        /// the appropriate flag is set in the CLI context and if the contracts were not cached and
        /// the compiler was invoked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiler_input: Option<CompilerInput>,
    },
    /// The compilation was ignored.
    Ignored {
        /// The reason behind the compilation being ignored.
        reason: String,
        /// Additional fields that describe more information on why the compilation is ignored.
        #[serde(flatten)]
        additional_fields: IndexMap<String, serde_json::Value>,
    },
}

/// Information about the compiled contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompiledContractInformation {
    /// The JSON contract ABI. This is only included if the appropriate flag is set in the CLI context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<JsonAbi>,
    /// The contract bytecode. This is only included if the appropriate flag is set in the CLI context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytecode: Option<String>,
    /// The hash of the bytecode.
    /// Note that it is the hash of the raw bytecode bytes (the decoded `bytecode` string)
    /// if `requires_linking` is false, otherwise it is the hash of the `bytecode` string.
    pub bytecode_hash: B256,
    /// Whether the bytecode contains unresolved library placeholders and requires linking.
    pub requires_linking: bool,
}

/// Information on each step in the execution.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StepReport {
    /// Information on the transactions submitted as part of this step.
    transactions: PlatformKeyedInformation<Vec<TransactionInformation>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionInformation {
    /// The hash of the transaction
    pub transaction_hash: TxHash,
    pub submission_timestamp: u64,
    pub block_timestamp: u64,
    pub block_number: BlockNumber,
}

/// Per-block metrics information for benchmark reporting. Each instance maps directly to an
/// (x, y) coordinate for graphing, with the block number or timestamp as x and various metrics
/// as y values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsInformation {
    pub block_number: u64,
    pub relative_block_number: u64,
    pub block_timestamp_seconds: u64,
    pub relative_block_timestamp_seconds: u64,
    pub observation_time_seconds: u64,
    pub relative_observation_time_seconds: u64,
    pub transaction_count: u64,
    pub step_count: BTreeMap<StepPath, u64>,

    // Rate fields - None for first block
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transactions_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_time_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_size_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_per_second: Option<f64>,

    // Gas fields
    pub block_gas_mined: u128,
    pub block_gas_limit: u128,
    pub block_gas_fullness: u64,

    // Substrate fields - None for non-substrate chains
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref_time: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref_time_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref_time_fullness: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_proof_size: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_proof_size_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_proof_size_fullness: Option<u64>,
}

/// Computes per-block [`MetricsInformation`] from a sorted slice of mined blocks.
///
/// Leading and trailing blocks with zero transactions are trimmed; interior empty blocks are
/// preserved. Rate fields (`transactions_per_second`, `gas_per_second`, etc.) are `None` for
/// the first retained block and computed as `f64` division for subsequent blocks.
pub fn compute_metrics_information(blocks: &[MinedBlockInformation]) -> Vec<MetricsInformation> {
    // Find the first and last block indices with transactions.
    let first = blocks
        .iter()
        .position(|b| !b.ethereum_block_information.transaction_hashes.is_empty());
    let last = blocks
        .iter()
        .rposition(|b| !b.ethereum_block_information.transaction_hashes.is_empty());

    let (first, last) = match (first, last) {
        (Some(f), Some(l)) => (f, l),
        _ => return Vec::new(),
    };

    let trimmed = &blocks[first..=last];
    let min_block_number = trimmed[0].ethereum_block_information.block_number;
    let min_timestamp = trimmed[0].ethereum_block_information.block_timestamp;
    let min_observation_time = trimmed[0]
        .observation_time
        .duration_since(UNIX_EPOCH)
        .expect("observation_time before UNIX_EPOCH")
        .as_secs();

    let mut result = Vec::with_capacity(trimmed.len());
    let mut prev_observation_time_millis: Option<u128> = None;

    for block in trimmed {
        let eth = &block.ethereum_block_information;
        let tx_count = eth.transaction_hashes.len() as u64;
        let this_ts = eth.block_timestamp;
        let obs_duration = block
            .observation_time
            .duration_since(UNIX_EPOCH)
            .expect("observation_time before UNIX_EPOCH");
        let this_obs = obs_duration.as_secs();
        let this_obs_millis = obs_duration.as_millis();

        let step_count = block
            .tx_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v as u64))
            .collect();

        // Compute rate fields using observation time deltas (millisecond precision)
        let (tps, gps, rt_ps, ps_ps) = if let Some(prev_obs_millis) = prev_observation_time_millis {
            let dt_millis = this_obs_millis.saturating_sub(prev_obs_millis);
            if dt_millis > 0 {
                let dt_f = dt_millis as f64 / 1000.0;
                let tps = Some(tx_count as f64 / dt_f);
                let gps = Some(eth.mined_gas as f64 / dt_f);
                let rt_ps = block
                    .substrate_block_information
                    .as_ref()
                    .map(|s| s.ref_time as f64 / dt_f);
                let ps_ps = block
                    .substrate_block_information
                    .as_ref()
                    .map(|s| s.proof_size as f64 / dt_f);
                (tps, gps, rt_ps, ps_ps)
            } else {
                (
                    Some(0.0),
                    Some(0.0),
                    block.substrate_block_information.as_ref().map(|_| 0.0),
                    block.substrate_block_information.as_ref().map(|_| 0.0),
                )
            }
        } else {
            (None, None, None, None)
        };

        let gas_fullness = if eth.block_gas_limit > 0 {
            (eth.mined_gas * 100 / eth.block_gas_limit) as u64
        } else {
            0
        };

        let (ref_time, ref_time_limit, ref_time_fullness) =
            if let Some(s) = &block.substrate_block_information {
                let fullness = if s.max_ref_time > 0 {
                    (s.ref_time * 100 / s.max_ref_time as u128) as u64
                } else {
                    0
                };
                (Some(s.ref_time), Some(s.max_ref_time), Some(fullness))
            } else {
                (None, None, None)
            };

        let (proof_size, proof_size_limit, proof_size_fullness) =
            if let Some(s) = &block.substrate_block_information {
                let fullness = if s.max_proof_size > 0 {
                    (s.proof_size * 100 / s.max_proof_size as u128) as u64
                } else {
                    0
                };
                (Some(s.proof_size), Some(s.max_proof_size), Some(fullness))
            } else {
                (None, None, None)
            };

        result.push(MetricsInformation {
            block_number: eth.block_number,
            relative_block_number: eth.block_number - min_block_number,
            block_timestamp_seconds: this_ts,
            relative_block_timestamp_seconds: this_ts - min_timestamp,
            observation_time_seconds: this_obs,
            relative_observation_time_seconds: this_obs - min_observation_time,
            transaction_count: tx_count,
            step_count,
            transactions_per_second: tps,
            gas_per_second: gps,
            ref_time_per_second: rt_ps,
            proof_size_per_second: ps_ps,
            block_gas_mined: eth.mined_gas,
            block_gas_limit: eth.block_gas_limit,
            block_gas_fullness: gas_fullness,
            block_ref_time: ref_time,
            block_ref_time_limit: ref_time_limit,
            block_ref_time_fullness: ref_time_fullness,
            block_proof_size: proof_size,
            block_proof_size_limit: proof_size_limit,
            block_proof_size_fullness: proof_size_fullness,
        });

        prev_observation_time_millis = Some(this_obs_millis);
    }

    result
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ContractInformation {
    /// The size of the contract on the various platforms.
    pub contract_size: PlatformKeyedInformation<usize>,
}

/// Information keyed by the platform identifier.
pub type PlatformKeyedInformation<T> = BTreeMap<PlatformIdentifier, T>;

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    #[allow(clippy::too_many_arguments)]
    fn make_block(
        block_number: u64,
        block_timestamp: u64,
        tx_count: usize,
        mined_gas: u128,
        gas_limit: u128,
        substrate: Option<SubstrateMinedBlockInformation>,
        tx_counts: BTreeMap<StepPath, usize>,
        observation_time: SystemTime,
    ) -> MinedBlockInformation {
        MinedBlockInformation {
            ethereum_block_information: EthereumMinedBlockInformation {
                block_number,
                block_timestamp,
                mined_gas,
                block_gas_limit: gas_limit,
                transaction_hashes: vec![TxHash::ZERO; tx_count],
            },
            substrate_block_information: substrate,
            tx_counts,
            observation_time,
        }
    }

    fn obs(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn simple_block(
        block_number: u64,
        block_timestamp: u64,
        tx_count: usize,
        observation_time: SystemTime,
    ) -> MinedBlockInformation {
        make_block(
            block_number,
            block_timestamp,
            tx_count,
            1000,
            10000,
            None,
            BTreeMap::new(),
            observation_time,
        )
    }

    #[test]
    fn empty_input() {
        let result = compute_metrics_information(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn all_zero_tx_blocks() {
        let blocks = vec![
            simple_block(1, 100, 0, obs(1000)),
            simple_block(2, 110, 0, obs(1010)),
            simple_block(3, 120, 0, obs(1020)),
        ];
        let result = compute_metrics_information(&blocks);
        assert!(result.is_empty());
    }

    #[test]
    fn single_block_with_transactions() {
        let blocks = vec![make_block(
            5,
            200,
            3,
            750,
            1000,
            None,
            BTreeMap::new(),
            obs(5000),
        )];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 1);
        let m = &result[0];
        assert_eq!(m.block_number, 5);
        assert_eq!(m.relative_block_number, 0);
        assert_eq!(m.block_timestamp_seconds, 200);
        assert_eq!(m.relative_block_timestamp_seconds, 0);
        assert_eq!(m.observation_time_seconds, 5000);
        assert_eq!(m.relative_observation_time_seconds, 0);
        assert_eq!(m.transaction_count, 3);
        assert!(m.transactions_per_second.is_none());
        assert!(m.gas_per_second.is_none());
        assert!(m.ref_time_per_second.is_none());
        assert!(m.proof_size_per_second.is_none());
        assert_eq!(m.block_gas_mined, 750);
        assert_eq!(m.block_gas_limit, 1000);
        assert_eq!(m.block_gas_fullness, 75);
    }

    #[test]
    fn leading_trailing_zero_tx_trimmed() {
        let blocks = vec![
            simple_block(1, 100, 0, obs(1000)),
            simple_block(2, 110, 0, obs(1010)),
            simple_block(3, 120, 5, obs(1020)),
            simple_block(4, 130, 3, obs(1030)),
            simple_block(5, 140, 0, obs(1040)),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].block_number, 3);
        assert_eq!(result[0].relative_block_number, 0);
        assert_eq!(result[0].relative_block_timestamp_seconds, 0);
        assert_eq!(result[0].relative_observation_time_seconds, 0);
        assert_eq!(result[0].transaction_count, 5);
        assert_eq!(result[1].block_number, 4);
        assert_eq!(result[1].relative_block_number, 1);
        assert_eq!(result[1].relative_block_timestamp_seconds, 10);
        assert_eq!(result[1].relative_observation_time_seconds, 10);
        assert_eq!(result[1].transaction_count, 3);
    }

    #[test]
    fn interior_zero_tx_blocks_preserved() {
        let blocks = vec![
            simple_block(1, 100, 5, obs(1000)),
            simple_block(2, 110, 0, obs(1010)),
            simple_block(3, 120, 3, obs(1020)),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].block_number, 1);
        assert_eq!(result[0].transaction_count, 5);
        assert_eq!(result[1].block_number, 2);
        assert_eq!(result[1].transaction_count, 0);
        assert_eq!(result[2].block_number, 3);
        assert_eq!(result[2].transaction_count, 3);
    }

    #[test]
    fn rate_computation() {
        let blocks = vec![
            make_block(1, 100, 5, 500, 10000, None, BTreeMap::new(), obs(1000)),
            make_block(2, 110, 20, 1000, 10000, None, BTreeMap::new(), obs(1010)),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 2);
        // First block: no rates
        assert!(result[0].transactions_per_second.is_none());
        assert!(result[0].gas_per_second.is_none());
        // Second block: 10s observation time delta
        assert_eq!(result[1].transactions_per_second.unwrap(), 2.0); // 20 / 10
        assert_eq!(result[1].gas_per_second.unwrap(), 100.0); // 1000 / 10
    }

    #[test]
    fn substrate_fields_populated() {
        let substrate = SubstrateMinedBlockInformation {
            ref_time: 5000,
            max_ref_time: 10000,
            proof_size: 3000,
            max_proof_size: 6000,
            block_hash: [0u8; 32],
        };
        let blocks = vec![
            make_block(
                1,
                100,
                2,
                500,
                1000,
                Some(substrate),
                BTreeMap::new(),
                obs(1000),
            ),
            make_block(
                2,
                110,
                3,
                700,
                1000,
                Some(substrate),
                BTreeMap::new(),
                obs(1010),
            ),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 2);
        // First block
        assert_eq!(result[0].block_ref_time, Some(5000));
        assert_eq!(result[0].block_ref_time_limit, Some(10000));
        assert_eq!(result[0].block_ref_time_fullness, Some(50));
        assert_eq!(result[0].block_proof_size, Some(3000));
        assert_eq!(result[0].block_proof_size_limit, Some(6000));
        assert_eq!(result[0].block_proof_size_fullness, Some(50));
        assert!(result[0].ref_time_per_second.is_none());
        assert!(result[0].proof_size_per_second.is_none());
        // Second block: rates computed over 10s observation time delta
        assert_eq!(result[1].ref_time_per_second.unwrap(), 500.0); // 5000 / 10
        assert_eq!(result[1].proof_size_per_second.unwrap(), 300.0); // 3000 / 10
    }

    #[test]
    fn substrate_fields_none_for_non_substrate() {
        let blocks = vec![simple_block(1, 100, 3, obs(1000))];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 1);
        assert!(result[0].block_ref_time.is_none());
        assert!(result[0].block_ref_time_limit.is_none());
        assert!(result[0].block_ref_time_fullness.is_none());
        assert!(result[0].block_proof_size.is_none());
        assert!(result[0].block_proof_size_limit.is_none());
        assert!(result[0].block_proof_size_fullness.is_none());
        assert!(result[0].ref_time_per_second.is_none());
        assert!(result[0].proof_size_per_second.is_none());
    }

    #[test]
    fn step_count_mapping() {
        let mut tx_counts = BTreeMap::new();
        let step_a: StepPath = "0".parse().unwrap();
        let step_b: StepPath = "1.0".parse().unwrap();
        tx_counts.insert(step_a.clone(), 5);
        tx_counts.insert(step_b.clone(), 3);

        let blocks = vec![make_block(
            1,
            100,
            8,
            1000,
            10000,
            None,
            tx_counts,
            obs(1000),
        )];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 1);
        assert_eq!(*result[0].step_count.get(&step_a).unwrap(), 5);
        assert_eq!(*result[0].step_count.get(&step_b).unwrap(), 3);
    }

    #[test]
    fn gas_fullness_computation() {
        let blocks = vec![make_block(
            1,
            100,
            1,
            75,
            100,
            None,
            BTreeMap::new(),
            obs(1000),
        )];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].block_gas_fullness, 75);
    }

    #[test]
    fn relative_values_across_blocks() {
        let blocks = vec![
            simple_block(10, 1000, 2, obs(5000)),
            simple_block(12, 1005, 3, obs(5005)),
            simple_block(15, 1020, 1, obs(5020)),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 3);
        // First block anchors
        assert_eq!(result[0].block_number, 10);
        assert_eq!(result[0].relative_block_number, 0);
        assert_eq!(result[0].block_timestamp_seconds, 1000);
        assert_eq!(result[0].relative_block_timestamp_seconds, 0);
        assert_eq!(result[0].observation_time_seconds, 5000);
        assert_eq!(result[0].relative_observation_time_seconds, 0);
        // Second block
        assert_eq!(result[1].block_number, 12);
        assert_eq!(result[1].relative_block_number, 2);
        assert_eq!(result[1].block_timestamp_seconds, 1005);
        assert_eq!(result[1].relative_block_timestamp_seconds, 5);
        assert_eq!(result[1].observation_time_seconds, 5005);
        assert_eq!(result[1].relative_observation_time_seconds, 5);
        // Third block
        assert_eq!(result[2].block_number, 15);
        assert_eq!(result[2].relative_block_number, 5);
        assert_eq!(result[2].block_timestamp_seconds, 1020);
        assert_eq!(result[2].relative_block_timestamp_seconds, 20);
        assert_eq!(result[2].observation_time_seconds, 5020);
        assert_eq!(result[2].relative_observation_time_seconds, 20);
    }

    #[test]
    fn rate_uses_observation_time_not_block_timestamp() {
        // Both blocks share the same block_timestamp (e.g. manual-seal mode),
        // but observation times are 10s apart. Rates should use observation time.
        let blocks = vec![
            make_block(1, 100, 5, 500, 10000, None, BTreeMap::new(), obs(1000)),
            make_block(2, 100, 20, 2000, 10000, None, BTreeMap::new(), obs(1010)),
        ];
        let result = compute_metrics_information(&blocks);

        assert_eq!(result.len(), 2);
        // First block: no rates
        assert!(result[0].transactions_per_second.is_none());
        assert!(result[0].gas_per_second.is_none());
        // Second block: rates based on 10s observation time delta, NOT block timestamp delta (0)
        assert_eq!(result[1].transactions_per_second.unwrap(), 2.0); // 20 / 10
        assert_eq!(result[1].gas_per_second.unwrap(), 200.0); // 2000 / 10
        // Verify the block timestamps are indeed the same
        assert_eq!(result[0].block_timestamp_seconds, 100);
        assert_eq!(result[1].block_timestamp_seconds, 100);
        assert_eq!(result[1].relative_block_timestamp_seconds, 0);
        // But observation times differ
        assert_eq!(result[0].observation_time_seconds, 1000);
        assert_eq!(result[1].observation_time_seconds, 1010);
        assert_eq!(result[1].relative_observation_time_seconds, 10);
    }
}
