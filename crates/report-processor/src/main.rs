use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::Display,
    fs::{File, OpenOptions, read_to_string},
    io::BufReader,
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

use revive_dt_common::types::{Mode, ParsedMode, ParsedTestSpecifier};
use revive_dt_report::{Report, TestCaseStatus};
use strum::EnumString;

use crate::{
    compare_hashes::{build_comparison_summary, compare_hashes},
    export_hashes::{HashData, extract_hashes},
};

mod compare_hashes;
mod export_hashes;

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

            write_or_overwrite_json(&output_file, &expectations)?;
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
                merged
                    .metadata_files
                    .extend(report.metadata_files.iter().cloned());

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

            let output_file = std::io::BufWriter::new(
                OpenOptions::new()
                    .truncate(true)
                    .create(true)
                    .write(true)
                    .open(&output_path)
                    .context("Failed to create the output file")?,
            );
            serde_json::to_writer(output_file, &merged)
                .context("Failed to write the merged report to file")?;
        }
        Cli::GenerateBenchmarksHtmlReport {
            report,
            report_url,
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

            let mut injections = Vec::new();

            if let Some(url) = &report_url {
                injections.push(format!("const REPORT_URL=\"{url}\";"));
            } else {
                let report_base64 = gzip_base64(&*report).context("Failed to embed report")?;
                injections.push(format!("const EMBEDDED_REPORT=\"{report_base64}\";"));
            }

            let metadata_base64 = gzip_base64(
                &report
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
            injections.push(format!("const EMBEDDED_METADATA=\"{metadata_base64}\";"));

            let injection = injections.join("");
            let html = TEMPLATE.replacen(
                INJECT_BEFORE,
                &format!("<script>{injection}</script>\n{INJECT_BEFORE}"),
                1,
            );

            std::fs::write(&output_path, &html).with_context(|| {
                format!("Failed to write HTML report to {}", output_path.display())
            })?;
        }
        Cli::ExportHashes {
            report,
            output_path,
            remove_prefix,
            platform_label,
        } => {
            let remove_prefix = remove_prefix
                .canonicalize()
                .context("Failed to canonicalize the remove-prefix path")?;

            let platform_hash_data = extract_hashes(&report, &remove_prefix, &platform_label)?;
            write_or_overwrite_json(&output_path, &platform_hash_data)?;

            println!(
                "Exported {} hashes across {} modes to {}.\nModes:\n- {}",
                platform_hash_data.count_hashes(),
                platform_hash_data.hashes.len(),
                output_path.display(),
                platform_hash_data
                    .hashes
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n- ")
            );
        }
        Cli::CompareHashes {
            hashes,
            modes,
            output_path,
        } => {
            let hashes: Vec<HashData> = hashes
                .into_iter()
                .map(|json_file| json_file.into_inner())
                .collect();

            let explicit_modes: Option<Vec<String>> = modes.map(|parsed_modes| {
                ParsedMode::many_to_modes(parsed_modes.iter())
                    .map(|mode| mode.to_string())
                    .collect()
            });

            let result = compare_hashes(&hashes, explicit_modes.as_deref())?;
            let summary = build_comparison_summary(&result);
            println!("{summary}");
            write_or_overwrite_json(&output_path, &result)?;
            println!(
                "Full comparison result written to: {}",
                output_path.display()
            );

            if result.count_mismatches() > 0 {
                bail!("Mismatches detected");
            }
        }
    };

    Ok(())
}

/// Creates the file at `output_path` and writes `value` as pretty-printed JSON,
/// or overwrites the file if it already exists.
fn write_or_overwrite_json(output_path: &Path, value: &impl Serialize) -> Result<()> {
    let output_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(output_path)
        .context("Failed to create output file")?;
    serde_json::to_writer_pretty(output_file, value).context("Failed to write JSON to output file")
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
        /// 2. The ancestor's of the provided path already exist such that no directory creations
        ///    are required.
        #[clap(long, verbatim_doc_comment)]
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
        /// The path of the report's JSON file. Metadata is always extracted
        /// from this file. The report data itself is embedded inline unless
        /// --report-url is also provided, in which case the HTML fetches the
        /// report from the URL at runtime instead.
        #[clap(long = "report-path")]
        report: JsonFile<Report>,

        /// Optional URL to the gzip-compressed JSON report. When provided,
        /// the HTML fetches the report from this URL at runtime instead of
        /// embedding it inline. Metadata is still embedded from --report-path.
        #[clap(long)]
        report_url: Option<String>,

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

    /// Extracts and exports the bytecode hashes from pre-link compilations from a [`Report`].
    ExportHashes {
        /// The path to the report's JSON file.
        #[clap(long = "report-path")]
        report: JsonFile<Report>,

        /// The path of the output JSON file to generate.
        #[clap(long)]
        output_path: PathBuf,

        /// The relative or absolute prefix path to remove from each source path found
        /// in the [`Report`] when added to the exported file. The path is resolved to
        /// its absolute canonical form before stripping. This is essential for normalization
        /// if the hashes are used for cross-platform comparison. For cross-platform comparison,
        /// this value should be the path to the base directory shared by all contracts,
        /// e.g. "/home/runner/work/contracts" or "C:\\Users\\runner\\work\\contracts".
        ///
        /// Note that the normalized paths added to the exported file should thereby not
        /// be used to access the filesystem.
        #[clap(long)]
        remove_prefix: PathBuf,

        /// The platform to be associated with the hashes (e.g. "linux" or "macos"),
        /// indicating which platform the hashes were generated on. This is included
        /// in the exported file.
        #[clap(long)]
        platform_label: String,
    },

    /// Compares hashes from multiple platforms and reports mismatches.
    CompareHashes {
        /// The paths to the files containing the hashes.
        #[clap(long = "hash-path")]
        hashes: Vec<JsonFile<HashData>>,

        /// The mode(s) to compare across the files provided.
        /// If omitted, the union of all modes found in the files will be compared.
        #[clap(short = 'm', long = "mode")]
        modes: Option<Vec<ParsedMode>>,

        /// The path of the output JSON file to write the comparison result.
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
        let file = BufReader::new(File::open(&path).context("Failed to open the file")?);
        serde_json::from_reader(file)
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

impl<T> JsonFile<T> {
    pub fn into_inner(self) -> T {
        *self.content
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
