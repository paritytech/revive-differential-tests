use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::Display,
    fs::{File, OpenOptions, read_to_string},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flate2::{Compression, write::GzEncoder};

use anyhow::{Context as _, Error, Result, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use revive_dt_common::types::{Mode, ParsedTestSpecifier};
use revive_dt_report::{Report, TestCaseStatus};
use strum::EnumString;

fn main() -> Result<()> {
    let cli = Cli::try_parse().context("Failed to parse the CLI arguments")?;

    match cli {
        Cli::GenerateExpectationsFile {
            report: report_path,
            output_path: output_file,
            remove_prefix,
            include_status,
        } => {
            let remove_prefix = remove_prefix
                .into_iter()
                .map(|path| path.canonicalize().context("Failed to canonicalize path"))
                .collect::<Result<Vec<_>>>()?;
            let include_status =
                include_status.map(|value| value.into_iter().collect::<HashSet<_>>());

            let expectations = report_path
                .execution_information
                .iter()
                .flat_map(|(metadata_file_path, metadata_file_report)| {
                    metadata_file_report
                        .case_reports
                        .iter()
                        .map(move |(case_idx, case_report)| {
                            (metadata_file_path, case_idx, case_report)
                        })
                })
                .flat_map(|(metadata_file_path, case_idx, case_report)| {
                    case_report.mode_execution_reports.iter().map(
                        move |(mode, execution_report)| {
                            (
                                metadata_file_path,
                                case_idx,
                                mode,
                                execution_report.status.as_ref(),
                            )
                        },
                    )
                })
                .filter_map(|(metadata_file_path, case_idx, mode, status)| {
                    status.map(|status| (metadata_file_path, case_idx, mode, status))
                })
                .map(|(metadata_file_path, case_idx, mode, status)| {
                    (
                        TestSpecifier {
                            metadata_file_path: Cow::Borrowed(
                                remove_prefix
                                    .iter()
                                    .filter_map(|prefix| {
                                        metadata_file_path.as_inner().strip_prefix(prefix).ok()
                                    })
                                    .next()
                                    .unwrap_or(metadata_file_path.as_inner()),
                            ),
                            case_idx: case_idx.into_inner(),
                            mode: Cow::Borrowed(mode),
                        },
                        Status::from(status),
                    )
                })
                .filter(|(_, status)| {
                    include_status
                        .as_ref()
                        .map(|allowed_status| allowed_status.contains(status))
                        .unwrap_or(true)
                })
                .collect::<Expectations>();

            let output_file = OpenOptions::new()
                .truncate(true)
                .create(true)
                .write(true)
                .open(output_file)
                .context("Failed to create the output file")?;
            serde_json::to_writer_pretty(output_file, &expectations)
                .context("Failed to write the expectations to file")?;
        }
        Cli::CompareExpectationFiles {
            base_expectations: base_expectation_path,
            other_expectations: other_expectation_path,
        } => {
            let keys = base_expectation_path
                .keys()
                .chain(other_expectation_path.keys())
                .collect::<BTreeSet<_>>();

            for key in keys {
                let base_status = base_expectation_path.get(key).context(format!(
                    "Entry not found in the base expectations: \"{}\"",
                    key
                ))?;
                let other_status = other_expectation_path.get(key).context(format!(
                    "Entry not found in the other expectations: \"{}\"",
                    key
                ))?;

                if base_status != other_status {
                    bail!(
                        "Expectations for entry \"{}\" have changed. They were {:?} and now they are {:?}",
                        key,
                        base_status,
                        other_status
                    )
                }
            }
        }
        Cli::MergeReports {
            reports,
            output_path,
        } => {
            let mut reports = reports.into_iter();
            let first = reports.next().context("At least one report is required")?;
            let mut merged = Report {
                context: first.content.context.clone(),
                metadata_files: first.content.metadata_files.clone(),
                execution_information: first.content.execution_information.clone(),
            };

            for report in reports {
                merged.metadata_files.extend(report.metadata_files.iter().cloned());

                for (metadata_path, file_report) in &report.execution_information {
                    let merged_file_report = merged
                        .execution_information
                        .entry(metadata_path.clone())
                        .or_default();

                    for (mode, compilation_report) in &file_report.compilation_reports {
                        merged_file_report
                            .compilation_reports
                            .entry(mode.clone())
                            .or_insert_with(|| compilation_report.clone());
                    }

                    for (case_idx, case_report) in &file_report.case_reports {
                        let merged_case = merged_file_report
                            .case_reports
                            .entry(*case_idx)
                            .or_default();

                        for (mode, exec_report) in &case_report.mode_execution_reports {
                            let merged_exec = merged_case
                                .mode_execution_reports
                                .entry(mode.clone())
                                .or_default();

                            if merged_exec.status.is_none() {
                                merged_exec.status.clone_from(&exec_report.status);
                            }

                            merged_exec
                                .metrics_information
                                .extend(exec_report.metrics_information.clone());
                            merged_exec
                                .platform_execution
                                .extend(exec_report.platform_execution.clone());
                            merged_exec
                                .mined_block_information
                                .extend(exec_report.mined_block_information.clone());

                            for (path, contracts) in &exec_report.compiled_contracts {
                                merged_exec
                                    .compiled_contracts
                                    .entry(path.clone())
                                    .or_default()
                                    .extend(contracts.clone());
                            }

                            for (instance, platforms) in &exec_report.contract_addresses {
                                merged_exec
                                    .contract_addresses
                                    .entry(instance.clone())
                                    .or_default()
                                    .extend(platforms.clone());
                            }

                            for (step_path, step_report) in &exec_report.steps {
                                merged_exec
                                    .steps
                                    .entry(step_path.clone())
                                    .or_insert_with(|| step_report.clone());
                            }
                        }
                    }
                }
            }

            let output_file = OpenOptions::new()
                .truncate(true)
                .create(true)
                .write(true)
                .open(&output_path)
                .context("Failed to create the output file")?;
            serde_json::to_writer_pretty(output_file, &merged)
                .context("Failed to write the merged report to file")?;
        }
        Cli::GenerateBenchmarksHtmlReport {
            report: report_path,
            output_path,
        } => {
            const TEMPLATE: &str = include_str!("../../../assets/benchmark-report.html");
            const INJECT_BEFORE: &str = "<script>\n// ═══════════════════════════════════════════════════════════════════════════\n// Constants";

            if !TEMPLATE.contains(INJECT_BEFORE) {
                anyhow::bail!(
                    "Injection marker not found in HTML template. \
                     Expected to find: {:?}",
                    INJECT_BEFORE
                );
            }

            let report_base64 = gzip_base64(&*report_path).context("Failed to embed report")?;

            let metadata_base64 = gzip_base64(
                &report_path
                    .metadata_files
                    .iter()
                    .map(|path| {
                        let path_ref: &Path = path.as_ref();
                        let key = path_ref.display().to_string();
                        let value: serde_json::Value = serde_json::from_str(
                            &read_to_string(path_ref)
                                .with_context(|| format!("Failed to read metadata file: {key}"))?,
                        )
                        .with_context(|| format!("Failed to parse metadata file as JSON: {key}"))?;
                        Ok((key, value))
                    })
                    .collect::<Result<serde_json::Map<String, serde_json::Value>>>()?,
            )
            .context("Failed to embed metadata")?;

            let html = TEMPLATE.replacen(
                INJECT_BEFORE,
                &format!(
                    "<script>const EMBEDDED_REPORT=\"{report_base64}\";const EMBEDDED_METADATA=\"{metadata_base64}\";</script>\n{INJECT_BEFORE}"
                ),
                1,
            );

            std::fs::write(&output_path, &html).with_context(|| {
                format!("Failed to write HTML report to {}", output_path.display())
            })?;
        }
    };

    Ok(())
}

/// Serialize `value` as JSON, gzip-compress it at maximum compression, and return a standard
/// base64-encoded string. The browser decompresses this with the native
/// `DecompressionStream('gzip')` API.
fn gzip_base64(value: &impl Serialize) -> Result<String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    serde_json::to_writer(&mut encoder, value).context("Failed to serialize into gzip encoder")?;
    let compressed = encoder.finish().context("Failed to finalize gzip stream")?;
    Ok(STANDARD.encode(compressed))
}

type Expectations<'a> = BTreeMap<TestSpecifier<'a>, Status>;

/// A tool that's used to process the reports generated by the retester binary in various ways.
#[derive(Clone, Debug, Parser)]
#[command(name = "retester", term_width = 100)]
pub enum Cli {
    /// Generates an expectation file out of a given report.
    GenerateExpectationsFile {
        /// The path of the report's JSON file to generate the expectation's file for.
        #[clap(long = "report-path")]
        report: JsonFile<Report>,

        /// The path of the output file to generate.
        ///
        /// Note that we expect that:
        /// 1. The provided path points to a JSON file.
        /// 1. The ancestor's of the provided path already exist such that no directory creations
        ///    are required.
        #[clap(long)]
        output_path: PathBuf,

        /// Prefix paths to remove from the paths in the final expectations file.
        #[clap(long)]
        remove_prefix: Vec<PathBuf>,

        /// Controls which test case statuses are included in the generated expectations file. If
        /// nothing is specified then it will include all of the test case status.
        #[clap(long)]
        include_status: Option<Vec<Status>>,
    },

    /// Compares two expectation files to ensure that they match each other.
    CompareExpectationFiles {
        /// The path of the base expectation file.
        #[clap(long = "base-expectation-path")]
        base_expectations: JsonFile<Expectations<'static>>,

        /// The path of the other expectation file.
        #[clap(long = "other-expectation-path")]
        other_expectations: JsonFile<Expectations<'static>>,
    },

    /// Generates an HTML report for the provided benchmark report.
    GenerateBenchmarksHtmlReport {
        /// The path of the report's JSON file to generate the HTML report for.
        #[clap(long = "report-path")]
        report: JsonFile<Report>,

        /// The path of the output file to output the HTML report to.
        #[clap(long)]
        output_path: PathBuf,
    },

    /// Merges multiple report JSON files into a single combined report.
    ///
    /// The context is taken from the first report. Execution information from all reports is
    /// merged together: for the same metadata file and case, platform-keyed data from later
    /// reports is added alongside data from earlier reports.
    MergeReports {
        /// The paths of the report JSON files to merge.
        #[clap(long = "report-path", required = true)]
        reports: Vec<JsonFile<Report>>,

        /// The path of the merged output report JSON file.
        #[clap(long)]
        output_path: PathBuf,
    },
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    EnumString,
)]
#[strum(serialize_all = "kebab-case")]
pub enum Status {
    Succeeded,
    Failed,
    Ignored,
}

impl From<TestCaseStatus> for Status {
    fn from(value: TestCaseStatus) -> Self {
        match value {
            TestCaseStatus::Succeeded { .. } => Self::Succeeded,
            TestCaseStatus::Failed { .. } => Self::Failed,
            TestCaseStatus::Ignored { .. } => Self::Ignored,
        }
    }
}

impl<'a> From<&'a TestCaseStatus> for Status {
    fn from(value: &'a TestCaseStatus) -> Self {
        match value {
            TestCaseStatus::Succeeded { .. } => Self::Succeeded,
            TestCaseStatus::Failed { .. } => Self::Failed,
            TestCaseStatus::Ignored { .. } => Self::Ignored,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsonFile<T> {
    path: PathBuf,
    content: Box<T>,
}

impl<T> Deref for JsonFile<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl<T> DerefMut for JsonFile<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.content
    }
}

impl<T> FromStr for JsonFile<T>
where
    T: DeserializeOwned,
{
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(s);
        let file = File::open(&path).context("Failed to open the file")?;
        serde_json::from_reader(&file)
            .map(|content| Self { path, content })
            .context(format!(
                "Failed to deserialize file's content as {}",
                std::any::type_name::<T>()
            ))
    }
}

impl<T> Display for JsonFile<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.path.display(), f)
    }
}

impl<T> From<JsonFile<T>> for String {
    fn from(value: JsonFile<T>) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TestSpecifier<'a> {
    pub metadata_file_path: Cow<'a, Path>,
    pub case_idx: usize,
    pub mode: Cow<'a, Mode>,
}

impl<'a> Display for TestSpecifier<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}::{}::{}",
            self.metadata_file_path.display(),
            self.case_idx,
            self.mode
        )
    }
}

impl<'a> From<TestSpecifier<'a>> for ParsedTestSpecifier {
    fn from(
        TestSpecifier {
            metadata_file_path,
            case_idx,
            mode,
        }: TestSpecifier,
    ) -> Self {
        Self::CaseWithMode {
            metadata_file_path: metadata_file_path.to_path_buf(),
            case_idx,
            mode: mode.into_owned(),
        }
    }
}

impl TryFrom<ParsedTestSpecifier> for TestSpecifier<'static> {
    type Error = Error;

    fn try_from(value: ParsedTestSpecifier) -> Result<Self> {
        let ParsedTestSpecifier::CaseWithMode {
            metadata_file_path,
            case_idx,
            mode,
        } = value
        else {
            bail!("Expected a full test case specifier")
        };
        Ok(Self {
            metadata_file_path: Cow::Owned(metadata_file_path),
            case_idx,
            mode: Cow::Owned(mode),
        })
    }
}

impl<'a> Serialize for TestSpecifier<'a> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

impl<'d, 'a> Deserialize<'d> for TestSpecifier<'a> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'d>,
    {
        let string = String::deserialize(deserializer)?;
        let mut splitted = string.split("::");
        let (Some(metadata_file_path), Some(case_idx), Some(mode), None) = (
            splitted.next(),
            splitted.next(),
            splitted.next(),
            splitted.next(),
        ) else {
            return Err(serde::de::Error::custom(
                "Test specifier doesn't contain the components required",
            ));
        };
        let metadata_file_path = PathBuf::from(metadata_file_path);
        let case_idx = usize::from_str(case_idx)
            .map_err(|_| serde::de::Error::custom("Case idx is not a usize"))?;
        let mode = Mode::from_str(mode).map_err(|_| serde::de::Error::custom("Invalid mode"))?;

        Ok(Self {
            metadata_file_path: Cow::Owned(metadata_file_path),
            case_idx,
            mode: Cow::Owned(mode),
        })
    }
}
