use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::Display,
    fs::{File, OpenOptions},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context as _, Error, Result, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use revive_dt_common::types::{Mode, ParsedMode, ParsedTestSpecifier};
use revive_dt_report::{Report, TestCaseStatus};
use strum::EnumString;

use crate::{
    compare_hashes::compare_hashes,
    export_hashes::{HashData, extract_hashes},
};

mod compare_hashes;
mod export_hashes;

fn main() -> Result<()> {
    let cli = Cli::try_parse().context("Failed to parse the CLI arguments")?;

    match cli {
        Cli::GenerateExpectationsFile {
            report_path,
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

            let output_file = write_or_overwrite_file(&output_file)?;
            serde_json::to_writer_pretty(output_file, &expectations)
                .context("Failed to write the expectations to file")?;
        }
        Cli::CompareExpectationFiles {
            base_expectation_path,
            other_expectation_path,
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
        Cli::ExportHashes {
            report_path,
            output_path,
            remove_prefix,
            platform_label,
        } => {
            let platform_hash_data = extract_hashes(&report_path, &remove_prefix, &platform_label);
            let output_file = write_or_overwrite_file(&output_path)?;
            serde_json::to_writer_pretty(&output_file, &platform_hash_data)
                .context("Failed to write the hashes to file")?;

            println!(
                "Exported {} hashes across {} modes ({}) to {}",
                platform_hash_data.count_hashes(),
                platform_hash_data.hashes.len(),
                platform_hash_data
                    .hashes
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                output_path.display()
            );
        }
        Cli::CompareHashes { hash_paths, modes } => {
            let hashes: Vec<HashData> = hash_paths
                .into_iter()
                .map(|json_file| json_file.into_inner())
                .collect();

            let explicit_modes: Option<Vec<String>> = modes.map(|parsed_modes| {
                ParsedMode::many_to_modes(parsed_modes.iter())
                    .map(|mode| mode.to_string())
                    .collect()
            });

            compare_hashes(&hashes, explicit_modes.as_deref())?;

            // TODO.
            println!("Comparing {} files", hashes.len());
        }
    };

    Ok(())
}

fn write_or_overwrite_file(output_path: &Path) -> Result<File, Error> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(output_path)
        .context("Failed to create output file")
}

type Expectations<'a> = BTreeMap<TestSpecifier<'a>, Status>;

/// A tool that's used to process the reports generated by the retester binary in various ways.
#[derive(Clone, Debug, Parser)]
#[command(name = "retester", term_width = 100)]
pub enum Cli {
    /// Generates an expectation file out of a given report.
    GenerateExpectationsFile {
        /// The path of the report's JSON file to generate the expectation's file for.
        #[clap(long)]
        report_path: JsonFile<Report>,

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
        #[clap(long)]
        base_expectation_path: JsonFile<Expectations<'static>>,

        /// The path of the other expectation file.
        #[clap(long)]
        other_expectation_path: JsonFile<Expectations<'static>>,
    },

    /// Extracts and exports the bytecode hashes from a [`Report`].
    ExportHashes {
        /// The path to the report's JSON file.
        #[clap(long)]
        report_path: JsonFile<Report>,

        /// The path of the output file to generate.
        ///
        /// Note the following expectations:
        /// 1. The provided path points to a JSON file.
        /// 2. The ancestors of the provided path already exist such that no directory creations
        ///    are required.
        #[clap(long, verbatim_doc_comment)]
        output_path: PathBuf,

        /// The absolute prefix path to remove from each source path found in the [`Report`]
        /// when added to the exported file. This is essential for normalization if the hashes
        /// are used for cross-platform comparison. For cross-platform comparison, this
        /// value should be the path to the base directory shared by all contracts,
        /// e.g. "/home/runner/work/contracts" or "C:\\Users\\runner\\work\\contracts".
        ///
        /// Note that the normalized paths added to the exported file should thereby not
        /// be used to access the filesystem.
        #[clap(long)]
        remove_prefix: PathBuf,

        /// The platform to be associated with the hashes (e.g. "linux" or "macos").
        #[clap(long)]
        platform_label: String,
    },

    /// Compares hashes from multiple platforms and reports mismatches.
    CompareHashes {
        /// The paths to the files containing the hashes.
        #[clap(long = "hash-path")]
        hash_paths: Vec<JsonFile<HashData>>,

        /// The mode(s) to compare across the files provided.
        /// If omitted, the union of all modes found in the files will be compared.
        #[clap(short = 'm', long = "mode")]
        modes: Option<Vec<ParsedMode>>,
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

impl<T> JsonFile<T> {
    pub fn into_inner(self) -> T {
        *self.content
    }

    pub fn path(&self) -> &Path {
        &self.path
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
