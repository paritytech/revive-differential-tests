use std::collections::BTreeMap;
use std::sync::Arc;
use std::{borrow::Cow, path::Path};

use anyhow::Context as _;
use futures::{Stream, StreamExt, stream};
use indexmap::{IndexMap, indexmap};
use revive_dt_common::cached_fs::read_to_string;
use revive_dt_common::types::PlatformIdentifier;
use revive_dt_config::{Context, IgnoreCasesConfiguration};
use revive_dt_format::corpus::Corpus;
use serde_json::{Value, json};

use revive_dt_compiler::Mode;
use revive_dt_compiler::SolidityCompiler;
use revive_dt_format::{
    case::{Case, CaseIdx},
    metadata::MetadataFile,
};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::{ExecutionSpecificReporter, Report, Reporter, TestCaseStatus};
use revive_dt_report::{TestSpecificReporter, TestSpecifier};
use tracing::{debug, error, info};

use crate::Platform;
use crate::helpers::NodePool;

pub async fn create_test_definitions_stream<'a>(
    // This is only required for creating the compiler objects and is not used anywhere else in the
    // function.
    context: &Context,
    corpus: &'a Corpus,
    platforms_and_nodes: &'a BTreeMap<PlatformIdentifier, (&dyn Platform, NodePool)>,
    test_case_ignore_configuration: &TestCaseIgnoreResolvedConfiguration,
    reporter: Reporter,
) -> impl Stream<Item = TestDefinition<'a>> {
    let cloned_reporter = reporter.clone();
    stream::iter(
        corpus
            .cases_iterator()
            .inspect(move |(metadata_file, ..)| {
                cloned_reporter
                    .report_metadata_file_discovery_event(
                        metadata_file.metadata_file_path.clone(),
                        metadata_file.content.clone(),
                    )
                    .unwrap();
            })
            .map(move |(metadata_file, case_idx, case, mode)| {
                let reporter = reporter.clone();

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
        match test.check_compatibility(test_case_ignore_configuration) {
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

#[derive(Clone, Debug, Default)]
pub struct TestCaseIgnoreResolvedConfiguration {
    /// Some existing report which will be used for a number of purposes.
    pub report: Option<Report>,

    /// Controls if test cases which succeeded from the existing report should
    /// be ignored or not.
    pub ignore_succeeding_test_cases_from_report: bool,

    /// Controls if test cases which contain steps expected to fail should be
    /// ignored or not.
    pub ignore_cases_with_failing_steps: bool,
}

impl TestCaseIgnoreResolvedConfiguration {
    pub fn with_report(self, report: impl Into<Option<Report>>) -> Self {
        self.mutate(|this| this.report = report.into())
    }

    pub fn with_ignore_succeeding_test_cases_from_report(
        self,
        ignore_succeeding_test_cases_from_report: bool,
    ) -> Self {
        self.mutate(|this| {
            this.ignore_succeeding_test_cases_from_report = ignore_succeeding_test_cases_from_report
        })
    }

    pub fn with_ignore_cases_with_failing_steps(
        self,
        ignore_cases_with_failing_steps: bool,
    ) -> Self {
        self.mutate(|this| this.ignore_cases_with_failing_steps = ignore_cases_with_failing_steps)
    }

    fn mutate(mut self, mutator: impl FnOnce(&mut Self)) -> Self {
        mutator(&mut self);
        self
    }
}

impl TryFrom<IgnoreCasesConfiguration> for TestCaseIgnoreResolvedConfiguration {
    type Error = anyhow::Error;

    fn try_from(value: IgnoreCasesConfiguration) -> Result<Self, Self::Error> {
        let mut this =
            Self::default().with_ignore_cases_with_failing_steps(value.cases_with_failing_steps);

        this = if let Some(succeeding_cases_from_report) = value.succeeding_cases_from_report {
            let content = read_to_string(succeeding_cases_from_report)
                .context("Failed to read report at the specified path")?;
            let report =
                serde_json::from_str::<Report>(&content).context("Failed to deserialize report")?;
            this.with_report(report)
                .with_ignore_succeeding_test_cases_from_report(true)
        } else {
            this
        };

        Ok(this)
    }
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
    pub fn check_compatibility(
        &self,
        test_case_ignore_configuration: &TestCaseIgnoreResolvedConfiguration,
    ) -> TestCheckFunctionResult {
        self.check_metadata_file_ignored()?;
        self.check_case_file_ignored()?;
        self.check_target_compatibility()?;
        self.check_evm_version_compatibility()?;
        self.check_compiler_compatibility()?;
        self.check_ignore_configuration(test_case_ignore_configuration)?;
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
        // The case targets takes presence over the metadata targets.
        let Some(targets) = self
            .case
            .targets
            .as_ref()
            .or(self.metadata.targets.as_ref())
        else {
            return Ok(());
        };

        let mut error_map = indexmap! {
            "test_desired_targets" => json!(targets),
        };

        let mut is_allowed = true;
        for (_, platform_information) in self.platforms.iter() {
            let is_allowed_for_platform =
                targets.contains(&platform_information.platform.vm_identifier());
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

    /// Checks if the test case should be executed or not based on the passed report and whether the
    /// user has instructed the tool to ignore the already succeeding test cases.
    fn check_ignore_configuration(
        &self,
        test_case_ignore_configuration: &TestCaseIgnoreResolvedConfiguration,
    ) -> TestCheckFunctionResult {
        // Check if the test case should be ignored based on it containing
        // failing steps.
        if test_case_ignore_configuration.ignore_cases_with_failing_steps
            && self.case.any_step_expected_to_fail()
        {
            return Err((
                "Ignored since it contains steps which are expected to fail",
                indexmap! {},
            ));
        }

        // Check if the succeeding cases from the provided report should be
        // ignored or not.
        if let (Some(report), true) = (
            test_case_ignore_configuration.report.as_ref(),
            test_case_ignore_configuration.ignore_succeeding_test_cases_from_report,
        ) {
            let test_case_status = report
                .execution_information
                .get(&(self.metadata_file_path.to_path_buf().into()))
                .and_then(|obj| obj.case_reports.get(&self.case_idx))
                .and_then(|obj| obj.mode_execution_reports.get(&self.mode))
                .and_then(|obj| obj.status.as_ref());

            match test_case_status {
                Some(TestCaseStatus::Failed { .. }) | None => {}
                Some(TestCaseStatus::Ignored { .. }) => {
                    return Err((
                        "Ignored since it was ignored in a previous run",
                        indexmap! {},
                    ));
                }
                Some(TestCaseStatus::Succeeded { .. }) => {
                    return Err(("Ignored since it succeeded in a prior run", indexmap! {}));
                }
            }
        }

        Ok(())
    }
}

pub struct TestPlatformInformation<'a> {
    pub platform: &'a dyn Platform,
    pub node: &'a dyn EthereumNode,
    pub compiler: Box<dyn SolidityCompiler>,
    pub reporter: ExecutionSpecificReporter,
}

type TestCheckFunctionResult = Result<(), (&'static str, IndexMap<&'static str, Value>)>;
