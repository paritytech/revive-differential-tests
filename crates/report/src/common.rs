//! Common types and functions used throughout the crate.

use std::path::PathBuf;

use revive_dt_common::define_wrapper_type;
use revive_dt_compiler::Mode;
use revive_dt_format::case::CaseIdx;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

define_wrapper_type!(
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct MetadataFilePath(PathBuf);
);

/// An absolute specifier for a test.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct TestSpecifier {
    #[serde_as(as = "DisplayFromStr")]
    pub solc_mode: Mode,
    pub metadata_file_path: PathBuf,
    pub case_idx: CaseIdx,
}
