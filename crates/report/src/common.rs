//! Common types and functions used throughout the crate.

use std::{path::PathBuf, sync::Arc};

use revive_dt_common::define_wrapper_type;
use revive_dt_compiler::Mode;
use revive_dt_format::{case::CaseIdx, input::StepIdx};
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
/// and whether it's the leader or follower.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExecutionSpecifier {
    pub test_specifier: Arc<TestSpecifier>,
    pub node_id: usize,
    pub node_designation: NodeDesignation,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeDesignation {
    Leader,
    Follower,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StepExecutionSpecifier {
    pub execution_specifier: Arc<ExecutionSpecifier>,
    pub step_idx: StepIdx,
}
