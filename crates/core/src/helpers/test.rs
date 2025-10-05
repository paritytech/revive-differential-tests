use std::collections::BTreeMap;
use std::sync::Arc;
use std::{borrow::Cow, path::Path};

use futures::{Stream, StreamExt, stream};
use indexmap::{IndexMap, indexmap};
use revive_dt_common::iterators::EitherIter;
use revive_dt_common::types::PlatformIdentifier;
use revive_dt_config::Context;
use revive_dt_format::mode::ParsedMode;
use serde_json::{Value, json};

use revive_dt_compiler::Mode;
use revive_dt_compiler::SolidityCompiler;
use revive_dt_format::{
    case::{Case, CaseIdx},
    metadata::MetadataFile,
};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::{ExecutionSpecificReporter, Reporter};
use revive_dt_report::{TestSpecificReporter, TestSpecifier};
use tracing::{debug, error, info};

use crate::Platform;
use crate::helpers::NodePool;

pub async fn create_test_definitions_stream<'a>(
    // This is only required for creating the compiler objects and is not used anywhere else in the
    // function.
    context: &Context,
    metadata_files: impl IntoIterator<Item = &'a MetadataFile>,
    platforms_and_nodes: &'a BTreeMap<PlatformIdentifier, (&dyn Platform, NodePool)>,
    reporter: Reporter,
) -> impl Stream<Item = TestDefinition<'a>> {
    stream::iter(
        metadata_files
            .into_iter()
            // Flatten over the cases.
            .flat_map(|metadata_file| {
                metadata_file
                    .cases
                    .iter()
                    .enumerate()
                    .map(move |(case_idx, case)| (metadata_file, case_idx, case))
            })
            // Flatten over the modes, prefer the case modes over the metadata file modes.
            .flat_map(move |(metadata_file, case_idx, case)| {
                let reporter = reporter.clone();

                let modes = case.modes.as_ref().or(metadata_file.modes.as_ref());
                let modes = match modes {
                    Some(modes) => EitherIter::A(
                        ParsedMode::many_to_modes(modes.iter()).map(Cow::<'static, _>::Owned),
                    ),
                    None => EitherIter::B(Mode::all().map(Cow::<'static, _>::Borrowed)),
                };

                modes.into_iter().map(move |mode| {
                    (
                        metadata_file,
                        case_idx,
                        case,
                        mode.clone(),
                        reporter.test_specific_reporter(Arc::new(TestSpecifier {
                            solc_mode: mode.as_ref().clone(),
                            metadata_file_path: metadata_file.metadata_file_path.clone(),
                            case_idx: CaseIdx::new(case_idx),
                        })),
                    )
                })
            })
            // Inform the reporter of each one of the test cases that were discovered which we expect to
            // run.
            .inspect(|(_, _, _, _, reporter)| {
                reporter
                    .report_test_case_discovery_event()
                    .expect("Can't fail");
            }),
    )
    // Creating the Test Definition objects from all of the various objects we have and creating
    // their required dependencies (e.g., compiler).
    .filter_map(
        move |(metadata_file, case_idx, case, mode, reporter)| async move {
            let mut platforms = BTreeMap::new();
            for (platform, node_pool) in platforms_and_nodes.values() {
                let node = node_pool.round_robbin();
                let compiler = platform
                    .new_compiler(context.clone(), mode.version.clone().map(Into::into))
                    .await
                    .inspect_err(|err| {
                        error!(
                            ?err,
                            platform_identifier = %platform.platform_identifier(),
                            "Failed to instantiate the compiler"
                        )
                    })
                    .ok()?;

                reporter
                    .report_node_assigned_event(
                        node.id(),
                        platform.platform_identifier(),
                        node.connection_string(),
                    )
                    .expect("Can't fail");

                let reporter =
                    reporter.execution_specific_reporter(node.id(), platform.platform_identifier());

                platforms.insert(
                    platform.platform_identifier(),
                    TestPlatformInformation {
                        platform: *platform,
                        node,
                        compiler,
                        reporter,
                    },
                );
            }

            Some(TestDefinition {
                /* Metadata file information */
                metadata: metadata_file,
                metadata_file_path: metadata_file.metadata_file_path.as_path(),

                /* Mode Information */
                mode: mode.clone(),

                /* Case Information */
                case_idx: CaseIdx::new(case_idx),
                case,

                /* Platform and Node Assignment Information */
                platforms,

                /* Reporter */
                reporter,
            })
        },
    )
    // Filter out the test cases which are incompatible or that can't run in the current setup.
    .filter_map(move |test| async move {
        match test.check_compatibility() {
            Ok(()) => Some(test),
            Err((reason, additional_information)) => {
                debug!(
                    metadata_file_path = %test.metadata.metadata_file_path.display(),
                    case_idx = %test.case_idx,
                    mode = %test.mode,
                    reason,
                    additional_information =
                        serde_json::to_string(&additional_information).unwrap(),
                    "Ignoring Test Case"
                );
                test.reporter
                    .report_test_ignored_event(
                        reason.to_string(),
                        additional_information
                            .into_iter()
                            .map(|(k, v)| (k.into(), v))
                            .collect::<IndexMap<_, _>>(),
                    )
                    .expect("Can't fail");
                None
            }
        }
    })
    .inspect(|test| {
        info!(
            metadata_file_path = %test.metadata_file_path.display(),
            case_idx = %test.case_idx,
            mode = %test.mode,
            "Created a test case definition"
        );
    })
}

/// This is a full description of a differential test to run alongside the full metadata file, the
/// specific case to be tested, the platforms that the tests should run on, the specific nodes of
/// these platforms that they should run on, the compilers to use, and everything else needed making
/// it a complete description.
pub struct TestDefinition<'a> {
    /* Metadata file information */
    pub metadata: &'a MetadataFile,
    pub metadata_file_path: &'a Path,

    /* Mode Information */
    pub mode: Cow<'a, Mode>,

    /* Case Information */
    pub case_idx: CaseIdx,
    pub case: &'a Case,

    /* Platform and Node Assignment Information */
    pub platforms: BTreeMap<PlatformIdentifier, TestPlatformInformation<'a>>,

    /* Reporter */
    pub reporter: TestSpecificReporter,
}

impl<'a> TestDefinition<'a> {
    /// Checks if this test can be ran with the current configuration.
    pub fn check_compatibility(&self) -> TestCheckFunctionResult {
        self.check_metadata_file_ignored()?;
        self.check_case_file_ignored()?;
        self.check_target_compatibility()?;
        self.check_evm_version_compatibility()?;
        self.check_compiler_compatibility()?;
        Ok(())
    }

    /// Checks if the metadata file is ignored or not.
    fn check_metadata_file_ignored(&self) -> TestCheckFunctionResult {
        if self.metadata.ignore.is_some_and(|ignore| ignore) {
            Err(("Metadata file is ignored.", indexmap! {}))
        } else {
            Ok(())
        }
    }

    /// Checks if the case file is ignored or not.
    fn check_case_file_ignored(&self) -> TestCheckFunctionResult {
        if self.case.ignore.is_some_and(|ignore| ignore) {
            Err(("Case is ignored.", indexmap! {}))
        } else {
            Ok(())
        }
    }

    /// Checks if the platforms all support the desired targets in the metadata file.
    fn check_target_compatibility(&self) -> TestCheckFunctionResult {
        let mut error_map = indexmap! {
            "test_desired_targets" => json!(self.metadata.targets.as_ref()),
        };
        let mut is_allowed = true;
        for (_, platform_information) in self.platforms.iter() {
            let is_allowed_for_platform = match self.metadata.targets.as_ref() {
                None => true,
                Some(required_vm_identifiers) => {
                    required_vm_identifiers.contains(&platform_information.platform.vm_identifier())
                }
            };
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform_information.platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "One of the platforms do do not support the targets allowed by the test.",
                error_map,
            ))
        }
    }

    // Checks for the compatibility of the EVM version with the platforms specified.
    fn check_evm_version_compatibility(&self) -> TestCheckFunctionResult {
        let Some(evm_version_requirement) = self.metadata.required_evm_version else {
            return Ok(());
        };

        let mut error_map = indexmap! {
            "test_desired_evm_version" => json!(self.metadata.required_evm_version),
        };
        let mut is_allowed = true;
        for (_, platform_information) in self.platforms.iter() {
            let is_allowed_for_platform =
                evm_version_requirement.matches(&platform_information.node.evm_version());
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform_information.platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "EVM version is incompatible for the platforms specified",
                error_map,
            ))
        }
    }

    /// Checks if the platforms compilers support the mode that the test is for.
    fn check_compiler_compatibility(&self) -> TestCheckFunctionResult {
        let mut error_map = indexmap! {
            "test_desired_evm_version" => json!(self.metadata.required_evm_version),
        };
        let mut is_allowed = true;
        for (_, platform_information) in self.platforms.iter() {
            let is_allowed_for_platform = platform_information
                .compiler
                .supports_mode(self.mode.optimize_setting, self.mode.pipeline);
            is_allowed &= is_allowed_for_platform;
            error_map.insert(
                platform_information.platform.platform_identifier().into(),
                json!(is_allowed_for_platform),
            );
        }

        if is_allowed {
            Ok(())
        } else {
            Err((
                "Compilers do not support this mode either for the provided platforms.",
                error_map,
            ))
        }
    }
}

pub struct TestPlatformInformation<'a> {
    pub platform: &'a dyn Platform,
    pub node: &'a dyn EthereumNode,
    pub compiler: Box<dyn SolidityCompiler>,
    pub reporter: ExecutionSpecificReporter,
}

type TestCheckFunctionResult = Result<(), (&'static str, IndexMap<&'static str, Value>)>;
