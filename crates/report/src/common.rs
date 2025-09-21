//! Common types and functions used throughout the crate.

use std::{path::PathBuf, sync::Arc};

use revive_dt_common::{define_wrapper_type, types::PlatformIdentifier};
use revive_dt_compiler::Mode;
use revive_dt_format::{case::CaseIdx, steps::StepPath};
use serde::{Deserialize, Serialize};

define_wrapper_type!(
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct MetadataFilePath(PathBuf);
);

/// An absolute specifier for a test.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TestSpecifier {
    pub solc_mode: Mode,
    pub metadata_file_path: PathBuf,
    pub case_idx: CaseIdx,
}

/// An absolute path for a test that also includes information about the node that it's assigned to
/// and what platform it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExecutionSpecifier {
    pub test_specifier: Arc<TestSpecifier>,
    pub node_id: usize,
    pub platform_identifier: PlatformIdentifier,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StepExecutionSpecifier {
    pub execution_specifier: Arc<ExecutionSpecifier>,
    pub step_idx: StepPath,
}
