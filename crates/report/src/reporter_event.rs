//! A reporter event sent by the report aggregator to the various listeners.

use std::collections::BTreeMap;

use revive_dt_compiler::Mode;
use revive_dt_format::case::CaseIdx;

use crate::{CompilationStatus, MetadataFilePath, TestCaseStatus};

#[derive(Clone, Debug)]
pub enum ReporterEvent {
    /// An event sent by the reporter once an entire metadata file and solc mode combination has
    /// finished execution.
    MetadataFileSolcModeCombinationExecutionCompleted {
        /// The path of the metadata file.
        metadata_file_path: MetadataFilePath,
        /// The Solc mode that this metadata file was executed in.
        mode: Mode,
        /// The status of each one of the cases.
        case_status: BTreeMap<CaseIdx, TestCaseStatus>,
    },

    /// An event sent by the reporter once an entire metadata file and mode combination has
    /// finished pre-link-only compilation.
    MetadataFileModeCombinationCompilationCompleted {
        metadata_file_path: MetadataFilePath,
        compilation_status: BTreeMap<Mode, CompilationStatus>,
    },
}
