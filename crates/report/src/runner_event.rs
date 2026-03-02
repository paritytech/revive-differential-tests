//! The types associated with the events sent by the runner to the reporter.
#![allow(dead_code)]

use crate::internal_prelude::*;

revive_dt_proc_macros::define_runner_event! {
    /// An event type that's sent by the test runners/drivers to the report aggregator.
    pub(crate) enum RunnerEvent {
        // Events on the base Reporter — no specifier auto-filled.
        Reporter => {
            /// An event emitted by the reporter when it wishes to listen to events emitted by
            /// the aggregator.
            SubscribeToEvents {
                /// The channel that the aggregator is to send the receive side of the channel on.
                tx: oneshot::Sender<broadcast::Receiver<ReporterEvent>>,
            },
            /// An event emitted by runners when they've discovered a metadata file.
            MetadataFileDiscovery {
                /// The path of the metadata file discovered.
                path: MetadataFilePath,
                /// The content of the metadata file.
                metadata: Metadata,
            },
            /// Reports the completion of the run.
            Completion {},
        },

        // Events on TestSpecificReporter — test_specifier: Arc<TestSpecifier> auto-filled.
        TestSpecifier => {
            /// An event emitted by the runners when they discover a test case.
            TestCaseDiscovery {},
            /// An event emitted by the runners when a test case is ignored.
            TestIgnored {
                /// A reason for the test to be ignored.
                reason: String,
                /// Additional fields that describe more information on why the test was ignored.
                additional_fields: IndexMap<String, serde_json::Value>,
            },
            /// An event emitted by the runners when a test case has succeeded.
            TestSucceeded {
                /// The number of steps of the case that were executed by the driver.
                steps_executed: usize,
            },
            /// An event emitted by the runners when a test case has failed.
            TestFailed {
                /// A reason for the failure of the test.
                reason: String,
            },
            /// An event emitted when the test case is assigned a platform node.
            NodeAssigned {
                /// The ID of the node that this case is being executed on.
                id: usize,
                /// The identifier of the platform used.
                platform_identifier: PlatformIdentifier,
                /// The connection string of the node.
                connection_string: String,
            },
        },

        // Events on ExecutionSpecificReporter — execution_specifier: Arc<ExecutionSpecifier>
        // auto-filled.
        ExecutionSpecifier => {
            /// An event emitted by the runners when the compilation of the pre-link contracts
            /// has succeeded.
            PreLinkContractsCompilationSucceeded {
                /// The version of the compiler used to compile the contracts.
                compiler_version: Version,
                /// The path of the compiler used to compile the contracts.
                compiler_path: PathBuf,
                /// A flag of whether the contract bytecode and ABI were cached or if they were
                /// compiled anew.
                is_cached: bool,
                /// The input provided to the compiler — optional and not provided if the
                /// contracts were obtained from the cache.
                compiler_input: Option<CompilerInput>,
                /// The output of the compiler.
                compiler_output: CompilerOutput,
            },
            /// An event emitted by the runners when the compilation of the post-link contracts
            /// has succeeded.
            PostLinkContractsCompilationSucceeded {
                /// The version of the compiler used to compile the contracts.
                compiler_version: Version,
                /// The path of the compiler used to compile the contracts.
                compiler_path: PathBuf,
                /// A flag of whether the contract bytecode and ABI were cached or if they were
                /// compiled anew.
                is_cached: bool,
                /// The input provided to the compiler — optional and not provided if the
                /// contracts were obtained from the cache.
                compiler_input: Option<CompilerInput>,
                /// The output of the compiler.
                compiler_output: CompilerOutput,
            },
            /// An event emitted by the runners when the compilation of the pre-link contract
            /// has failed.
            PreLinkContractsCompilationFailed {
                /// The version of the compiler used to compile the contracts.
                compiler_version: Option<Version>,
                /// The path of the compiler used to compile the contracts.
                compiler_path: Option<PathBuf>,
                /// The input provided to the compiler — optional and not provided if the
                /// contracts were obtained from the cache.
                compiler_input: Option<CompilerInput>,
                /// The failure reason.
                reason: String,
            },
            /// An event emitted by the runners when the compilation of the post-link contract
            /// has failed.
            PostLinkContractsCompilationFailed {
                /// The version of the compiler used to compile the contracts.
                compiler_version: Option<Version>,
                /// The path of the compiler used to compile the contracts.
                compiler_path: Option<PathBuf>,
                /// The input provided to the compiler — optional and not provided if the
                /// contracts were obtained from the cache.
                compiler_input: Option<CompilerInput>,
                /// The failure reason.
                reason: String,
            },
            /// An event emitted by the runners when they've deployed a new contract.
            ContractDeployed {
                /// The instance name of the contract.
                contract_instance: ContractInstance,
                /// The address of the contract.
                address: Address,
            },
            /// An event emitted with information on a transaction that was submitted for a
            /// certain step of the execution.
            StepTransactionInformation {
                /// The path of the step that this transaction belongs to.
                step_path: StepPath,
                /// Information about the transaction.
                transaction_information: TransactionInformation,
            },
            /// An event emitted with information on a compiled contract.
            ContractInformation {
                /// The path of the solidity source code that contains the contract.
                source_code_path: PathBuf,
                /// The name of the contract.
                contract_name: String,
                /// The size of the contract.
                contract_size: usize,
            },
            /// An event emitted when a block has been mined.
            BlockMined {
                /// Information on the mined block.
                mined_block_information: MinedBlockInformation,
            },
        },
    }
}

impl Reporter {
    pub fn test_specific_reporter(
        &self,
        test_specifier: impl Into<Arc<TestSpecifier>>,
    ) -> TestSpecificReporter {
        TestSpecificReporter {
            reporter: self.clone(),
            test_specifier: test_specifier.into(),
        }
    }

    pub async fn subscribe(&self) -> anyhow::Result<broadcast::Receiver<ReporterEvent>> {
        let (tx, rx) = oneshot::channel::<broadcast::Receiver<ReporterEvent>>();
        self.report_subscribe_to_events_event(tx)
            .context("Failed to send subscribe request to reporter task")?;
        rx.await.map_err(Into::into)
    }
}

impl TestSpecificReporter {
    pub fn execution_specific_reporter(
        &self,
        node_id: impl Into<usize>,
        platform_identifier: impl Into<PlatformIdentifier>,
    ) -> ExecutionSpecificReporter {
        ExecutionSpecificReporter {
            reporter: self.reporter.clone(),
            execution_specifier: Arc::new(ExecutionSpecifier {
                test_specifier: self.test_specifier.clone(),
                node_id: node_id.into(),
                platform_identifier: platform_identifier.into(),
            }),
        }
    }
}
