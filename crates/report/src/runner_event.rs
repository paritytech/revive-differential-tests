//! The types associated with the events sent by the runner to the reporter.
#![allow(dead_code)]

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use alloy::primitives::Address;
use anyhow::Context as _;
use indexmap::IndexMap;
use revive_dt_common::types::PlatformIdentifier;
use revive_dt_compiler::{CompilerInput, CompilerOutput};
use revive_dt_format::metadata::ContractInstance;
use revive_dt_format::metadata::Metadata;
use revive_dt_format::steps::StepPath;
use semver::Version;
use tokio::sync::{broadcast, oneshot};

use crate::MinedBlockInformation;
use crate::TransactionInformation;
use crate::{
    CompilationSpecifier, ExecutionSpecifier, PreLinkCompilationSpecifier, ReporterEvent,
    TestSpecifier, common::MetadataFilePath,
};

/// Conditionally wraps a value, or returns it as is.
macro_rules! __maybe_wrap {
    ($value:expr, $wrapper:path) => {
        $wrapper($value)
    };
    ($value:expr) => {
        $value
    };
}

/// Generates a report method that emits an event, auto-filling the specifier from self.
/// Optionally wraps the specifier in if a wrapper path is provided.
macro_rules! __report_gen_emit_with_specifier {
    (
        $ident:ident,
        $variant_ident:ident,
        $specifier_field_on_self:ident,
        $specifier_field_on_event:ident
        $(, $specifier_wrapper:path)?;
        $( $bname:ident : $bty:ty, )*
        ;
        $( $aname:ident : $aty:ty, )*
    ) => {
        paste::paste! {
            pub fn [< report_ $variant_ident:snake _event >](
                &self
                $(, $bname: impl Into<$bty> )*
                $(, $aname: impl Into<$aty> )*
            ) -> anyhow::Result<()> {
                self.report([< $variant_ident Event >] {
                    $specifier_field_on_event: __maybe_wrap!(
                        self.$specifier_field_on_self.clone()
                        $(, $specifier_wrapper)?
                    )
                    $(, $bname: $bname.into() )*
                    $(, $aname: $aname.into() )*
                })
            }
        }
    };
}

/// Scans event fields looking for a matching specifier field name.
///
/// Each MATCH arm maps a specifier field on `self` (the reporter) to a specifier field
/// on the event enum variant. This allows for the event's field to have a different name
/// than the reporter's specifier field if needed (e.g., `specifier` instead of `test_specifier`).
///
/// To support a new specifier field, just add a corresponding MATCH arm.
macro_rules! __report_gen_scan_for_specifier {
    // MATCH: test_specifier (on self) -> test_specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        test_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        test_specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            test_specifier,
            test_specifier;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // MATCH: execution_specifier (on self) -> execution_specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        execution_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        execution_specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            execution_specifier,
            execution_specifier;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // MATCH: execution_specifier (on self) -> specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        execution_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            execution_specifier,
            specifier,
            $crate::CompilationSpecifier::Execution;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // MATCH: step_specifier (on self) -> step_specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        step_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        step_specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            step_specifier,
            step_specifier;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // MATCH: compilation_specifier (on self) -> compilation_specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        compilation_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        compilation_specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            compilation_specifier,
            compilation_specifier;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // MATCH: compilation_specifier (on self) -> specifier (on event).
    (
        $ident:ident,
        $variant_ident:ident,
        compilation_specifier;
        $( $before:ident : $bty:ty, )*
        ;
        specifier : $skip_ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_emit_with_specifier!(
            $ident,
            $variant_ident,
            compilation_specifier,
            specifier,
            $crate::CompilationSpecifier::PreLink;
            $( $before : $bty, )* ; $( $after : $aty, )*
        );
    };

    // RECURSIVE: Field doesn't match, continue scanning.
    (
        $ident:ident,
        $variant_ident:ident,
        $specifier_field_on_self:ident;
        $( $before:ident : $bty:ty, )*
        ;
        $name:ident : $ty:ty,
        $( $after:ident : $aty:ty, )*
        ;
    ) => {
        __report_gen_scan_for_specifier!(
            $ident,
            $variant_ident,
            $specifier_field_on_self;
            $( $before : $bty, )* $name : $ty,
            ;
            $( $after : $aty, )*
            ;
        );
    };

    // TERMINAL: No matching specifier found.
    (
        $ident:ident,
        $variant_ident:ident,
        $specifier_field_on_self:ident;
        $( $before:ident : $bty:ty, )*
        ;
        ;
    ) => {};
}

/// Entry point: Processes a variant and starts scanning for specifier fields.
macro_rules! __report_gen_for_variant {
    // Empty variant - no fields.
    (
        $ident:ident,
        $variant_ident:ident,
        $specifier_field_on_self:ident;
    ) => {};

    // Variant with fields - start scanning.
    (
        $ident:ident,
        $variant_ident:ident,
        $specifier_field_on_self:ident;
        $( $field_ident:ident : $field_ty:ty ),+ $(,)?
    ) => {
        __report_gen_scan_for_specifier!(
            $ident,
            $variant_ident,
            $specifier_field_on_self;
            ;
            $( $field_ident : $field_ty, )*
            ;
        );
    };
}

/// Defines the runner-event which is sent from the test runners to the report aggregator.
///
/// This macro defines a number of things related to the reporting infrastructure and the interface
/// used. First of all, it defines the enum of all of the possible events that the runners can send
/// to the aggregator. For each one of the variants it defines a separate struct for it to allow the
/// variant field in the enum to be put in a [`Box`].
///
/// In addition to the above, it defines [`From`] implementations for the various event types for
/// the [`RunnerEvent`] enum essentially allowing for events such as [`CorpusFileDiscoveryEvent`] to
/// be converted into a [`RunnerEvent`].
///
/// In addition to the above, it also defines the [`RunnerEventReporter`] which is a wrapper around
/// an [`UnboundedSender`] allowing for events to be sent to the report aggregator.
///
/// With the above description, we can see that this macro defines almost all of the interface of
/// the reporting infrastructure, from the enum itself, to its associated types, and also to the
/// reporter that's used to report events to the aggregator.
///
/// [`UnboundedSender`]: tokio::sync::mpsc::UnboundedSender
macro_rules! define_event {
    (
        $(#[$enum_meta: meta])*
        $vis: vis enum $ident: ident {
            $(
                $(#[$variant_meta: meta])*
                $variant_ident: ident {
                    $(
                        $(#[$field_meta: meta])*
                        $field_ident: ident: $field_ty: ty
                    ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        paste::paste! {
            $(#[$enum_meta])*
            #[derive(Debug)]
            $vis enum $ident {
                $(
                    $(#[$variant_meta])*
                    $variant_ident(Box<[<$variant_ident Event>]>)
                ),*
            }

            impl $ident {
                pub fn variant_name(&self) -> &'static str {
                    match self {
                        $(
                            Self::$variant_ident { .. } => stringify!($variant_ident)
                        ),*
                    }
                }
            }

            $(
                #[derive(Debug)]
                $(#[$variant_meta])*
                $vis struct [<$variant_ident Event>] {
                    $(
                        $(#[$field_meta])*
                        $vis $field_ident: $field_ty
                    ),*
                }
            )*

            $(
                impl From<[<$variant_ident Event>]> for $ident {
                    fn from(value: [<$variant_ident Event>]) -> Self {
                        Self::$variant_ident(Box::new(value))
                    }
                }
            )*

            /// Provides a way to report events to the aggregator.
            ///
            /// Under the hood, this is a wrapper around an [`UnboundedSender`] which abstracts away
            /// the fact that channels are used and that implements high-level methods for reporting
            /// various events to the aggregator.
            #[derive(Clone, Debug)]
            pub struct [< $ident Reporter >]($vis tokio::sync::mpsc::UnboundedSender<$ident>);

            impl From<tokio::sync::mpsc::UnboundedSender<$ident>> for [< $ident Reporter >] {
                fn from(value: tokio::sync::mpsc::UnboundedSender<$ident>) -> Self {
                    Self(value)
                }
            }

            impl [< $ident Reporter >] {
                pub fn test_specific_reporter(
                    &self,
                    test_specifier: impl Into<std::sync::Arc<crate::common::TestSpecifier>>
                ) -> [< $ident TestSpecificReporter >] {
                    [< $ident TestSpecificReporter >] {
                        reporter: self.clone(),
                        test_specifier: test_specifier.into(),
                    }
                }

                pub fn pre_link_compilation_specific_reporter(
                    &self,
                    compilation_specifier: impl Into<std::sync::Arc<crate::common::PreLinkCompilationSpecifier>>
                ) -> [< $ident PreLinkCompilationSpecificReporter >] {
                    [< $ident PreLinkCompilationSpecificReporter >] {
                        reporter: self.clone(),
                        compilation_specifier: compilation_specifier.into(),
                    }
                }

                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.0.send(event.into()).map_err(Into::into)
                }

                $(
                    pub fn [< report_ $variant_ident:snake _event >](&self, $($field_ident: impl Into<$field_ty>),*) -> anyhow::Result<()> {
                        self.report([< $variant_ident Event >] {
                            $($field_ident: $field_ident.into()),*
                        })
                    }
                )*
            }

            /// A reporter that's tied to a specific test case.
            #[derive(Clone, Debug)]
            pub struct [< $ident TestSpecificReporter >] {
                $vis reporter: [< $ident Reporter >],
                $vis test_specifier: std::sync::Arc<crate::common::TestSpecifier>,
            }

            impl [< $ident TestSpecificReporter >] {
                pub fn execution_specific_reporter(
                    &self,
                    node_id: impl Into<usize>,
                    platform_identifier: impl Into<PlatformIdentifier>
                ) -> [< $ident ExecutionSpecificReporter >] {
                    [< $ident ExecutionSpecificReporter >] {
                        reporter: self.reporter.clone(),
                        execution_specifier: Arc::new($crate::common::ExecutionSpecifier {
                            test_specifier: self.test_specifier.clone(),
                            node_id: node_id.into(),
                            platform_identifier: platform_identifier.into(),
                        })
                    }
                }

                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.reporter.report(event)
                }

                $(
                    __report_gen_for_variant! {
                        $ident,
                        $variant_ident,
                        test_specifier;
                        $( $field_ident : $field_ty ),*
                    }
                )*
            }

            /// A reporter that's tied to a specific execution of the test case such as execution on
            /// a specific node from a specific platform.
            #[derive(Clone, Debug)]
            pub struct [< $ident ExecutionSpecificReporter >] {
                $vis reporter: [< $ident Reporter >],
                $vis execution_specifier: std::sync::Arc<$crate::common::ExecutionSpecifier>,
            }

            impl [< $ident ExecutionSpecificReporter >] {
                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.reporter.report(event)
                }

                $(
                    __report_gen_for_variant! {
                        $ident,
                        $variant_ident,
                        execution_specifier;
                        $( $field_ident : $field_ty ),*
                    }
                )*
            }

            /// A reporter that's tied to a specific step execution
            #[derive(Clone, Debug)]
            pub struct [< $ident StepExecutionSpecificReporter >] {
                $vis reporter: [< $ident Reporter >],
                $vis step_specifier: std::sync::Arc<$crate::common::StepExecutionSpecifier>,
            }

            impl [< $ident StepExecutionSpecificReporter >] {
                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.reporter.report(event)
                }

                $(
                    __report_gen_for_variant! {
                        $ident,
                        $variant_ident,
                        step_specifier;
                        $( $field_ident : $field_ty ),*
                    }
                )*
            }

            /// A reporter that's tied to a specific compilation.
            #[derive(Clone, Debug)]
            pub struct [< $ident PreLinkCompilationSpecificReporter >] {
                $vis reporter: [< $ident Reporter >],
                $vis compilation_specifier: std::sync::Arc<crate::common::PreLinkCompilationSpecifier>,
            }

            impl [< $ident PreLinkCompilationSpecificReporter >] {
                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.reporter.report(event)
                }

                $(
                    __report_gen_for_variant! {
                        $ident,
                        $variant_ident,
                        compilation_specifier;
                        $( $field_ident : $field_ty ),*
                    }
                )*
            }
        }
    };
}

define_event! {
    /// An event type that's sent by the test runners/drivers to the report aggregator.
    pub(crate) enum RunnerEvent {
        /// An event emitted by the reporter when it wishes to listen to events emitted by the
        /// aggregator.
        SubscribeToEvents {
            /// The channel that the aggregator is to send the receive side of the channel on.
            tx: oneshot::Sender<broadcast::Receiver<ReporterEvent>>
        },
        /// An event emitted by runners when they've discovered a metadata file.
        MetadataFileDiscovery {
            /// The path of the metadata file discovered.
            path: MetadataFilePath,
            /// The content of the metadata file.
            metadata: Metadata
        },
        /// An event emitted by the runners when they discover a test case.
        TestCaseDiscovery {
            /// A specifier for the test that was discovered.
            test_specifier: Arc<TestSpecifier>,
        },
        /// An event emitted by the runners when they discover a pre-link-only compilation.
        PreLinkCompilationDiscovery {
            /// A specifier for the compilation that was discovered.
            compilation_specifier: Arc<PreLinkCompilationSpecifier>,
        },
        /// An event emitted by the runners when a test case is ignored.
        TestIgnored {
            /// A specifier for the test that's been ignored.
            test_specifier: Arc<TestSpecifier>,
            /// A reason for the test to be ignored.
            reason: String,
            /// Additional fields that describe more information on why the test was ignored.
            additional_fields: IndexMap<String, serde_json::Value>
        },
        /// An event emitted by the runners when a test case has succeeded.
        TestSucceeded {
            /// A specifier for the test that succeeded.
            test_specifier: Arc<TestSpecifier>,
            /// The number of steps of the case that were executed by the driver.
            steps_executed: usize,
        },
        /// An event emitted by the runners when a test case has failed.
        TestFailed {
            /// A specifier for the test that succeeded.
            test_specifier: Arc<TestSpecifier>,
            /// A reason for the failure of the test.
            reason: String,
        },
        /// An event emitted when the test case is assigned a platform node.
        NodeAssigned {
            /// A specifier for the test that the assignment is for.
            test_specifier: Arc<TestSpecifier>,
            /// The ID of the node that this case is being executed on.
            id: usize,
            /// The identifier of the platform used.
            platform_identifier: PlatformIdentifier,
            /// The connection string of the node.
            connection_string: String,
        },
        /// An event emitted by the runners when the compilation of the contracts has succeeded
        /// on the pre-link contracts.
        PreLinkContractsCompilationSucceeded {
            /// A specifier for the compilation taking place.
            specifier: CompilationSpecifier,
            /// The version of the compiler used to compile the contracts.
            compiler_version: Version,
            /// The path of the compiler used to compile the contracts.
            compiler_path: PathBuf,
            /// A flag of whether the contract bytecode and ABI were cached or if they were compiled anew.
            is_cached: bool,
            /// The input provided to the compiler - this is optional and not provided if the
            /// contracts were obtained from the cache.
            compiler_input: Option<CompilerInput>,
            /// The output of the compiler.
            compiler_output: CompilerOutput
        },
        /// An event emitted by the runners when the compilation of the contracts has succeeded
        /// on the post-link contracts.
        PostLinkContractsCompilationSucceeded {
            /// A specifier for the compilation taking place in an execution context.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The version of the compiler used to compile the contracts.
            compiler_version: Version,
            /// The path of the compiler used to compile the contracts.
            compiler_path: PathBuf,
            /// A flag of whether the contract bytecode and ABI were cached or if they were compiled anew.
            is_cached: bool,
            /// The input provided to the compiler - this is optional and not provided if the
            /// contracts were obtained from the cache.
            compiler_input: Option<CompilerInput>,
            /// The output of the compiler.
            compiler_output: CompilerOutput
        },
        /// An event emitted by the runners when the compilation of the pre-link contract has failed.
        PreLinkContractsCompilationFailed {
            /// A specifier for the compilation taking place.
            specifier: CompilationSpecifier,
            /// The version of the compiler used to compile the contracts.
            compiler_version: Option<Version>,
            /// The path of the compiler used to compile the contracts.
            compiler_path: Option<PathBuf>,
            /// The input provided to the compiler - this is optional and not provided if the
            /// contracts were obtained from the cache.
            compiler_input: Option<CompilerInput>,
            /// The failure reason.
            reason: String,
        },
        /// An event emitted by the runners when the compilation of the post-link contract has failed.
        PostLinkContractsCompilationFailed {
            /// A specifier for the compilation taking place in an execution context.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The version of the compiler used to compile the contracts.
            compiler_version: Option<Version>,
            /// The path of the compiler used to compile the contracts.
            compiler_path: Option<PathBuf>,
            /// The input provided to the compiler - this is optional and not provided if the
            /// contracts were obtained from the cache.
            compiler_input: Option<CompilerInput>,
            /// The failure reason.
            reason: String,
        },
        /// An event emitted by the runners when a pre-link-only compilation is ignored.
        PreLinkContractsCompilationIgnored {
            /// A specifier for the compilation that has been ignored.
            compilation_specifier: Arc<PreLinkCompilationSpecifier>,
            /// A reason for the compilation to be ignored.
            reason: String,
            /// Additional fields that describe more information on why the compilation was ignored.
            additional_fields: IndexMap<String, serde_json::Value>
        },
        /// An event emitted by the runners when a library has been deployed.
        LibrariesDeployed {
            /// A specifier for the execution that's taking place.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The addresses of the libraries that were deployed.
            libraries: BTreeMap<ContractInstance, Address>
        },
        /// An event emitted by the runners when they've deployed a new contract.
        ContractDeployed {
            /// A specifier for the execution that's taking place.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The instance name of the contract.
            contract_instance: ContractInstance,
            /// The address of the contract.
            address: Address
        },
        /// Reports the completion of the run.
        Completion {},

        /* Benchmarks Events */
        /// An event emitted with information on a transaction that was submitted for a certain step
        /// of the execution.
        StepTransactionInformation {
            /// A specifier for the execution that's taking place.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The path of the step that this transaction belongs to.
            step_path: StepPath,
            /// Information about the transaction
            transaction_information: TransactionInformation
        },
        ContractInformation {
            /// A specifier for the execution that's taking place.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// The path of the solidity source code that contains the contract.
            source_code_path: PathBuf,
            /// The name of the contract
            contract_name: String,
            /// The size of the contract
            contract_size: usize
        },
        BlockMined {
            /// A specifier for the execution that's taking place.
            execution_specifier: Arc<ExecutionSpecifier>,
            /// Information on the mined block,
            mined_block_information: MinedBlockInformation
        }
    }
}

/// An extension to the [`Reporter`] implemented by the macro.
impl RunnerEventReporter {
    pub async fn subscribe(&self) -> anyhow::Result<broadcast::Receiver<ReporterEvent>> {
        let (tx, rx) = oneshot::channel::<broadcast::Receiver<ReporterEvent>>();
        self.report_subscribe_to_events_event(tx)
            .context("Failed to send subscribe request to reporter task")?;
        rx.await.map_err(Into::into)
    }
}

pub type Reporter = RunnerEventReporter;
pub type TestSpecificReporter = RunnerEventTestSpecificReporter;
pub type ExecutionSpecificReporter = RunnerEventExecutionSpecificReporter;
pub type PreLinkCompilationSpecificReporter = RunnerEventPreLinkCompilationSpecificReporter;

/// A wrapper that allows functions to accept either reporter type for compilation events.
pub enum CompilationReporter<'a> {
    Execution(&'a ExecutionSpecificReporter),
    PreLink(&'a PreLinkCompilationSpecificReporter),
}
