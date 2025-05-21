//! The reporter is the central place observing test execution by collecting data.
//!
//! The data collected gives useful insights into the outcome of the test run
//! and helps identifying and reproducing failing cases.

use std::{
    collections::HashMap,
    fs::File,
    path::PathBuf,
    sync::{LazyLock, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use revive_dt_config::{Arguments, TestingPlatform};
use revive_dt_format::{corpus::Corpus, mode::SolcMode};
use revive_solc_json_interface::{SolcStandardJsonInput, SolcStandardJsonOutput};

pub(crate) static REPORTER: LazyLock<Mutex<Report>> = LazyLock::new(Default::default);

/// The `Report` datastructure stores all relevant inforamtion required for generating reports.
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Report {
    /// The configuration used during the test.
    config: Arguments,
    /// The observed test corpora.
    corpora: Vec<Corpus>,
    /// The observed test definitions.
    metadata_files: Vec<PathBuf>,
    /// The observed compilation results.
    compilation_results: HashMap<TestingPlatform, Vec<CompilationResult>>,
}

impl Report {
    /// Add a compilation task to the report.
    pub fn compilation(span: Span, platform: TestingPlatform, compilation_task: CompilationTask) {
        REPORTER
            .lock()
            .unwrap()
            .compilation_results
            .entry(platform)
            .or_default()
            .push(CompilationResult {
                compilation_task,
                span,
            });
    }

    /// Write the report to disk.
    pub fn save() -> anyhow::Result<()> {
        REPORTER.lock().unwrap().write_to_file()
    }

    fn write_to_file(&self) -> anyhow::Result<()> {
        let file_name = format!(
            "{:?}.json",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
        );
        let file = File::create(self.config.directory().join(file_name))?;

        serde_json::to_writer_pretty(file, &self)?;

        Ok(())
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        let _ = self.write_to_file();
    }
}

/// Contains a compiled contract.
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
struct CompilationResult {
    /// The observed compilation task.
    compilation_task: CompilationTask,
    /// The linked span.
    span: Span,
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

impl Span {
    /// Create a new [Span] with case and input index at 0.
    pub fn new(corpus: Corpus) -> Self {
        let mut reporter = REPORTER.lock().unwrap();

        reporter.corpora.push(corpus);

        Self {
            corpus: reporter.corpora.len() - 1,
            metadata_file: 0,
            case: 0,
            input: 0,
        }
    }

    /// Advance to the next metadata file: Resets the case input index to 0.
    pub fn next_metadata(&mut self, metadata_file: PathBuf) {
        let mut reporter = REPORTER.lock().unwrap();

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
