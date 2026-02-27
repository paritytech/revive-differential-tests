//! Implementation of the report aggregator task which consumes the events sent by the various
//! reporters and combines them into a single unified report.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::OpenOptions,
    ops::{Add, Div},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    hex,
    json_abi::JsonAbi,
    primitives::{Address, B256, BlockNumber, BlockTimestamp, TxHash},
};
use anyhow::{Context as _, Result};
use indexmap::IndexMap;
use itertools::Itertools;
use revive_dt_common::types::PlatformIdentifier;
use revive_dt_compiler::{CompilerInput, CompilerOutput, Mode};
use revive_dt_config::{Context, HasReportConfiguration, HasWorkingDirectoryConfiguration};
use revive_dt_format::{case::CaseIdx, metadata::ContractInstance, steps::StepPath};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use sha2::{Digest, Sha256};
use tokio::sync::{
    broadcast::{Sender, channel},
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tracing::debug;

use crate::*;

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
                Context::ExportJsonSchema(_) | Context::ExportGenesis(..) => None,
                Context::Compile(ref context) => context.report.file_name.clone(),
            },
            report: Report::new(context),
            remaining_cases: Default::default(),
            remaining_compilation_modes: Default::default(),
            runner_tx: Some(runner_tx),
            runner_rx,
            listener_tx,
        }
    }

    pub fn into_task(mut self) -> (Reporter, impl Future<Output = Result<Report>>) {
        let reporter = self
            .runner_tx
            .take()
            .map(Into::into)
            .expect("Can't fail since this can only be called once");
        (reporter, async move { self.aggregate().await })
    }

    async fn aggregate(mut self) -> Result<Report> {
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
                RunnerEvent::PreLinkCompilationDiscovery(event) => {
                    self.handle_pre_link_compilation_discovery(*event);
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
                RunnerEvent::PreLinkContractsCompilationIgnored(event) => {
                    self.handle_pre_link_contracts_compilation_ignored_event(*event);
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

        Ok(self.report)
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
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .insert(event.test_specifier.case_idx);
    }

    fn handle_pre_link_compilation_discovery(&mut self, event: PreLinkCompilationDiscoveryEvent) {
        self.remaining_compilation_modes
            .entry(
                event
                    .compilation_specifier
                    .metadata_file_path
                    .clone()
                    .into(),
            )
            .or_default()
            .insert(event.compilation_specifier.solc_mode.clone());
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
            .entry(specifier.solc_mode.clone())
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
                    .get(&specifier.solc_mode)?
                    .status
                    .clone()
                    .expect("Can't be uninitialized");
                Some((*case_idx, case_status))
            })
            .collect::<BTreeMap<_, _>>();
        let event = ReporterEvent::MetadataFileSolcModeCombinationExecutionCompleted {
            metadata_file_path: specifier.metadata_file_path.clone().into(),
            mode: specifier.solc_mode.clone(),
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

        match &event.specifier {
            CompilationSpecifier::Execution(specifier) => {
                let execution_information = self.execution_information(specifier);
                execution_information.pre_link_compilation_status = Some(status);
            }
            CompilationSpecifier::PreLink(specifier) => {
                let report = self.pre_link_compilation_report(specifier);
                report.status = Some(status);
                self.handle_post_pre_link_contracts_compilation_status_update(specifier);
            }
        }
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

        let execution_information = self.execution_information(&event.execution_specifier);
        execution_information.post_link_compilation_status = Some(status);
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

        match &event.specifier {
            CompilationSpecifier::Execution(specifier) => {
                let execution_information = self.execution_information(specifier);
                execution_information.pre_link_compilation_status = Some(status);
            }
            CompilationSpecifier::PreLink(specifier) => {
                let report = self.pre_link_compilation_report(specifier);
                report.status = Some(status);
                self.handle_post_pre_link_contracts_compilation_status_update(specifier);
            }
        }
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

        let execution_information = self.execution_information(&event.execution_specifier);
        execution_information.post_link_compilation_status = Some(status);
    }

    fn handle_pre_link_contracts_compilation_ignored_event(
        &mut self,
        event: PreLinkContractsCompilationIgnoredEvent,
    ) {
        let status = CompilationStatus::Ignored {
            reason: event.reason,
            additional_fields: event.additional_fields,
        };

        let report = self.pre_link_compilation_report(&event.compilation_specifier);
        report.status = Some(status.clone());
        self.handle_post_pre_link_contracts_compilation_status_update(&event.compilation_specifier);
    }

    fn handle_post_pre_link_contracts_compilation_status_update(
        &mut self,
        specifier: &PreLinkCompilationSpecifier,
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
        self.execution_information(&event.execution_specifier)
            .deployed_libraries = Some(event.libraries);
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

                        // Computing the TPS.
                        let tps = block_information
                            .iter()
                            .tuple_windows::<(_, _)>()
                            .map(|(block1, block2)| {
                                block2.ethereum_block_information.transaction_hashes.len() as u64
                                    / (block2.ethereum_block_information.block_timestamp
                                        - block1.ethereum_block_information.block_timestamp)
                            })
                            .collect::<Vec<_>>();
                        report
                            .metrics
                            .get_or_insert_default()
                            .transaction_per_second
                            .with_list(*platform_identifier, tps);

                        // Computing the GPS.
                        let gps = block_information
                            .iter()
                            .tuple_windows::<(_, _)>()
                            .map(|(block1, block2)| {
                                block2.ethereum_block_information.mined_gas as u64
                                    / (block2.ethereum_block_information.block_timestamp
                                        - block1.ethereum_block_information.block_timestamp)
                            })
                            .collect::<Vec<_>>();
                        report
                            .metrics
                            .get_or_insert_default()
                            .gas_per_second
                            .with_list(*platform_identifier, gps);

                        // Computing the gas block fullness
                        let gas_block_fullness = block_information
                            .iter()
                            .map(|block| block.gas_block_fullness_percentage())
                            .map(|v| v as u64)
                            .collect::<Vec<_>>();
                        report
                            .metrics
                            .get_or_insert_default()
                            .gas_block_fullness
                            .with_list(*platform_identifier, gas_block_fullness);

                        // Computing the ref-time block fullness
                        let reftime_block_fullness = block_information
                            .iter()
                            .filter_map(|block| block.ref_time_block_fullness_percentage())
                            .map(|v| v as u64)
                            .collect::<Vec<_>>();
                        if !reftime_block_fullness.is_empty() {
                            report
                                .metrics
                                .get_or_insert_default()
                                .ref_time_block_fullness
                                .get_or_insert_default()
                                .with_list(*platform_identifier, reftime_block_fullness);
                        }

                        // Computing the proof size block fullness
                        let proof_size_block_fullness = block_information
                            .iter()
                            .filter_map(|block| block.proof_size_block_fullness_percentage())
                            .map(|v| v as u64)
                            .collect::<Vec<_>>();
                        if !proof_size_block_fullness.is_empty() {
                            report
                                .metrics
                                .get_or_insert_default()
                                .proof_size_block_fullness
                                .get_or_insert_default()
                                .with_list(*platform_identifier, proof_size_block_fullness);
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
            .entry(specifier.solc_mode.clone())
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

    fn pre_link_compilation_report(
        &mut self,
        specifier: &PreLinkCompilationSpecifier,
    ) -> &mut PreLinkCompilationReport {
        self.report
            .execution_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .compilation_reports
            .entry(specifier.solc_mode.clone())
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
            .entry(specifier.solc_mode.clone())
            .or_default()
            .remove(&specifier.case_idx);
    }

    /// Removes the compilation mode specified by the `specifier` from the tracked remaining compilation modes.
    fn remove_remaining_compilation_mode(&mut self, specifier: &PreLinkCompilationSpecifier) {
        self.remaining_compilation_modes
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .remove(&specifier.solc_mode);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    /// The context that the tool was started up with.
    pub context: Context,
    /// The list of metadata files that were found by the tool.
    pub metadata_files: BTreeSet<MetadataFilePath>,
    /// Metrics from the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    /// Information relating to each metadata file after executing the tool.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub execution_information: BTreeMap<MetadataFilePath, MetadataFileReport>,
}

impl Report {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            metrics: Default::default(),
            metadata_files: Default::default(),
            execution_information: Default::default(),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct MetadataFileReport {
    /// Metrics from the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    /// The report of each case keyed by the case idx.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub case_reports: BTreeMap<CaseIdx, CaseReport>,
    /// The [`CompilationReport`] for each of the [`Mode`]s.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[serde_as(as = "BTreeMap<DisplayFromStr, _>")]
    pub compilation_reports: BTreeMap<Mode, PreLinkCompilationReport>,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CaseReport {
    /// Metrics from the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    /// The [`ExecutionReport`] for each one of the [`Mode`]s.
    #[serde_as(as = "HashMap<DisplayFromStr, _>")]
    pub mode_execution_reports: HashMap<Mode, ExecutionReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ExecutionReport {
    /// Information on the status of the test case and whether it succeeded, failed, or was ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TestCaseStatus>,
    /// Metrics from the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
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
    /// Information on the deployed libraries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_libraries: Option<BTreeMap<ContractInstance, Address>>,
    /// Information on the deployed contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_contracts: Option<BTreeMap<ContractInstance, Address>>,
}

/// The pre-link-only compilation report.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PreLinkCompilationReport {
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

/// The metrics we collect for our benchmarks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub transaction_per_second: Metric<u64>,
    pub gas_per_second: Metric<u64>,
    /* Block Fullness */
    pub gas_block_fullness: Metric<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_time_block_fullness: Option<Metric<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_size_block_fullness: Option<Metric<u64>>,
}

/// The data that we store for a given metric (e.g., TPS).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metric<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<PlatformKeyedInformation<T>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<PlatformKeyedInformation<T>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean: Option<PlatformKeyedInformation<T>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median: Option<PlatformKeyedInformation<T>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<PlatformKeyedInformation<Vec<T>>>,
}

impl<T> Metric<T>
where
    T: Default
        + Copy
        + Ord
        + PartialOrd
        + Add<Output = T>
        + Div<Output = T>
        + TryFrom<usize, Error: std::fmt::Debug>,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn platform_identifiers(&self) -> BTreeSet<PlatformIdentifier> {
        self.minimum
            .as_ref()
            .map(|m| m.keys())
            .into_iter()
            .flatten()
            .chain(
                self.maximum
                    .as_ref()
                    .map(|m| m.keys())
                    .into_iter()
                    .flatten(),
            )
            .chain(self.mean.as_ref().map(|m| m.keys()).into_iter().flatten())
            .chain(self.median.as_ref().map(|m| m.keys()).into_iter().flatten())
            .chain(self.raw.as_ref().map(|m| m.keys()).into_iter().flatten())
            .copied()
            .collect()
    }

    pub fn with_list(
        &mut self,
        platform_identifier: PlatformIdentifier,
        original_list: Vec<T>,
    ) -> &mut Self {
        let mut list = original_list.clone();
        list.sort();
        let Some(min) = list.first().copied() else {
            return self;
        };
        let Some(max) = list.last().copied() else {
            return self;
        };
        let sum = list.iter().fold(T::default(), |acc, num| acc + *num);
        let mean = sum / TryInto::<T>::try_into(list.len()).unwrap();

        let median = match list.len().is_multiple_of(2) {
            true => {
                let idx = list.len() / 2;
                let val1 = *list.get(idx - 1).unwrap();
                let val2 = *list.get(idx).unwrap();
                (val1 + val2) / TryInto::<T>::try_into(2usize).unwrap()
            }
            false => {
                let idx = list.len() / 2;
                *list.get(idx).unwrap()
            }
        };

        self.minimum
            .get_or_insert_default()
            .insert(platform_identifier, min);
        self.maximum
            .get_or_insert_default()
            .insert(platform_identifier, max);
        self.mean
            .get_or_insert_default()
            .insert(platform_identifier, mean);
        self.median
            .get_or_insert_default()
            .insert(platform_identifier, median);
        self.raw
            .get_or_insert_default()
            .insert(platform_identifier, original_list);

        self
    }

    pub fn combine(&self, other: &Self) -> Self {
        let mut platform_identifiers = self.platform_identifiers();
        platform_identifiers.extend(other.platform_identifiers());

        let mut this = Self::new();
        for platform_identifier in platform_identifiers {
            let mut l1 = self
                .raw
                .as_ref()
                .and_then(|m| m.get(&platform_identifier))
                .cloned()
                .unwrap_or_default();
            let l2 = other
                .raw
                .as_ref()
                .and_then(|m| m.get(&platform_identifier))
                .cloned()
                .unwrap_or_default();
            l1.extend(l2);
            this.with_list(platform_identifier, l1);
        }

        this
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ContractInformation {
    /// The size of the contract on the various platforms.
    pub contract_size: PlatformKeyedInformation<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MinedBlockInformation {
    pub ethereum_block_information: EthereumMinedBlockInformation,
    pub substrate_block_information: Option<SubstrateMinedBlockInformation>,
    pub tx_counts: BTreeMap<StepPath, usize>,
}

impl MinedBlockInformation {
    pub fn gas_block_fullness_percentage(&self) -> u8 {
        self.ethereum_block_information
            .gas_block_fullness_percentage()
    }

    pub fn ref_time_block_fullness_percentage(&self) -> Option<u8> {
        self.substrate_block_information
            .as_ref()
            .map(|block| block.ref_time_block_fullness_percentage())
    }

    pub fn proof_size_block_fullness_percentage(&self) -> Option<u8> {
        self.substrate_block_information
            .as_ref()
            .map(|block| block.proof_size_block_fullness_percentage())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EthereumMinedBlockInformation {
    /// The block number.
    pub block_number: BlockNumber,

    /// The block timestamp.
    pub block_timestamp: BlockTimestamp,

    /// The amount of gas mined in the block.
    pub mined_gas: u128,

    /// The gas limit of the block.
    pub block_gas_limit: u128,

    /// The hashes of the transactions that were mined as part of the block.
    pub transaction_hashes: Vec<TxHash>,
}

impl EthereumMinedBlockInformation {
    pub fn gas_block_fullness_percentage(&self) -> u8 {
        (self.mined_gas * 100 / self.block_gas_limit) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubstrateMinedBlockInformation {
    /// The ref time for substrate based chains.
    pub ref_time: u128,

    /// The max ref time for substrate based chains.
    pub max_ref_time: u64,

    /// The proof size for substrate based chains.
    pub proof_size: u128,

    /// The max proof size for substrate based chains.
    pub max_proof_size: u64,
}

impl SubstrateMinedBlockInformation {
    pub fn ref_time_block_fullness_percentage(&self) -> u8 {
        (self.ref_time * 100 / self.max_ref_time as u128) as u8
    }

    pub fn proof_size_block_fullness_percentage(&self) -> u8 {
        (self.proof_size * 100 / self.max_proof_size as u128) as u8
    }
}

/// Information keyed by the platform identifier.
pub type PlatformKeyedInformation<T> = BTreeMap<PlatformIdentifier, T>;
