//! The reporter is the central place observing test execution by collecting data.
//!
//! The data collected gives useful insights into the outcome of the test run
//! and helps identifying and reproducing failing cases.

use std::{
    collections::HashMap,
    fs::{self, File, create_dir_all},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use revive_dt_config::{Arguments, TestingPlatform};
use revive_dt_format::{corpus::Corpus, mode::SolcMode};
use revive_solc_json_interface::{SolcStandardJsonInput, SolcStandardJsonOutput};

use crate::analyzer::CompilerStatistics;

pub(crate) static REPORTER: OnceLock<Mutex<Report>> = OnceLock::new();

/// The `Report` datastructure stores all relevant inforamtion required for generating reports.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    /// The configuration used during the test.
    pub config: Arguments,
    /// The observed test corpora.
    pub corpora: Vec<Corpus>,
    /// The observed test definitions.
    pub metadata_files: Vec<PathBuf>,
    /// The observed compilation results.
    pub compiler_results: HashMap<TestingPlatform, Vec<CompilationResult>>,
    /// The observed compilation statistics.
    pub compiler_statistics: HashMap<TestingPlatform, CompilerStatistics>,
    /// The file name this is serialized to.
    #[serde(skip)]
    directory: PathBuf,
}

/// Contains a compiled contract.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilationTask {
    /// The observed compiler input.
    pub json_input: SolcStandardJsonInput,
    /// The observed compiler output.
    pub json_output: Option<SolcStandardJsonOutput>,
    /// The observed compiler mode.
    pub mode: SolcMode,
    /// The observed compiler version.
    pub compiler_version: String,
    /// The observed error, if any.
    pub error: Option<String>,
}

/// Represents a report about a compilation task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilationResult {
    /// The observed compilation task.
    pub compilation_task: CompilationTask,
    /// The linked span.
    pub span: Span,
}

/// The [Span] struct indicates the context of what is being reported.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Span {
    /// The corpus index this belongs to.
    corpus: usize,
    /// The metadata file this belongs to.
    metadata_file: usize,
    /// The index of the case definition this belongs to.
    case: usize,
    /// The index of the case input this belongs to.
    input: usize,
}

impl Report {
    /// The file name where this report will be written to.
    pub const FILE_NAME: &str = "report.json";

    /// The [Span] is expected to initialize the reporter by providing the config.
    const INITIALIZED_VIA_SPAN: &str = "requires a Span which initializes the reporter";

    /// Create a new [Report].
    fn new(config: Arguments) -> anyhow::Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let directory = config.directory().join("report").join(format!("{now}"));
        if !directory.exists() {
            create_dir_all(&directory)?;
        }

        Ok(Self {
            config,
            directory,
            ..Default::default()
        })
    }

    /// Add a compilation task to the report.
    pub fn compilation(span: Span, platform: TestingPlatform, compilation_task: CompilationTask) {
        let mut report = REPORTER
            .get()
            .expect(Report::INITIALIZED_VIA_SPAN)
            .lock()
            .unwrap();

        report
            .compiler_statistics
            .entry(platform)
            .or_default()
            .sample(&compilation_task);

        report
            .compiler_results
            .entry(platform)
            .or_default()
            .push(CompilationResult {
                compilation_task,
                span,
            });
    }

    /// Write the report to disk.
    pub fn save() -> anyhow::Result<()> {
        let Some(reporter) = REPORTER.get() else {
            return Ok(());
        };
        let report = reporter.lock().unwrap();

        if let Err(error) = report.write_to_file() {
            anyhow::bail!("can not write report: {error}");
        }

        if report.config.extract_problems {
            if let Err(error) = report.save_compiler_problems() {
                anyhow::bail!("can not write compiler problems: {error}");
            }
        }

        Ok(())
    }

    /// Write compiler problems to disk for later debugging.
    pub fn save_compiler_problems(&self) -> anyhow::Result<()> {
        for (platform, results) in self.compiler_results.iter() {
            for result in results {
                // ignore if there were no errors
                if result.compilation_task.error.is_none()
                    && result
                        .compilation_task
                        .json_output
                        .as_ref()
                        .and_then(|output| output.errors.as_ref())
                        .map(|errors| errors.is_empty())
                        .unwrap_or(true)
                {
                    continue;
                }

                let path = &self.metadata_files[result.span.metadata_file]
                    .parent()
                    .unwrap()
                    .join(format!("{platform}_errors"));
                if !path.exists() {
                    create_dir_all(path)?;
                }

                if let Some(error) = result.compilation_task.error.as_ref() {
                    fs::write(path.join("compiler_error.txt"), error)?;
                }

                if let Some(errors) = result.compilation_task.json_output.as_ref() {
                    let file = File::create(path.join("compiler_output.txt"))?;
                    serde_json::to_writer_pretty(file, &errors)?;
                }
            }
        }

        Ok(())
    }

    fn write_to_file(&self) -> anyhow::Result<()> {
        let path = self.directory.join(Self::FILE_NAME);

        let file = File::create(&path).context(path.display().to_string())?;
        serde_json::to_writer_pretty(file, &self)?;

        log::info!("report written to: {}", path.display());

        Ok(())
    }
}

impl Span {
    /// Create a new [Span] with case and input index at 0.
    ///
    /// Initializes the reporting facility on the first call.
    pub fn new(corpus: Corpus, config: Arguments) -> anyhow::Result<Self> {
        let report = Mutex::new(Report::new(config)?);
        let mut reporter = REPORTER.get_or_init(|| report).lock().unwrap();
        reporter.corpora.push(corpus);

        Ok(Self {
            corpus: reporter.corpora.len() - 1,
            metadata_file: 0,
            case: 0,
            input: 0,
        })
    }

    /// Advance to the next metadata file: Resets the case input index to 0.
    pub fn next_metadata(&mut self, metadata_file: PathBuf) {
        let mut reporter = REPORTER
            .get()
            .expect(Report::INITIALIZED_VIA_SPAN)
            .lock()
            .unwrap();

        reporter.metadata_files.push(metadata_file);

        self.metadata_file = reporter.metadata_files.len() - 1;
        self.case = 0;
        self.input = 0;
    }

    /// Advance to the next case: Increas the case index by one and resets the input index to 0.
    pub fn next_case(&mut self) {
        self.case += 1;
        self.input = 0;
    }

    /// Advance to the next input.
    pub fn next_input(&mut self) {
        self.input += 1;
    }
}
