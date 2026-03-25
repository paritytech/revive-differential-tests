//! Common types and functions used throughout the crate.

use crate::internal_prelude::*;

define_wrapper_type!(
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct MetadataFilePath(PathBuf);
);

impl AsRef<Path> for MetadataFilePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// An absolute specifier for a test.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TestSpecifier {
    pub compiler_mode: Mode,
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

/// An absolute specifier for post-link-only compilation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PostLinkCompilationSpecifier {
    pub compiler_mode: Mode,
    pub metadata_file_path: PathBuf,
}

/// An absolute specifier for compilation events depending on the context.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompilationSpecifier {
    /// Compilation happening as part of test execution.
    Execution(Arc<ExecutionSpecifier>),
    /// Post-link-only compilation happening without test execution.
    PostLink(Arc<PostLinkCompilationSpecifier>),
}
