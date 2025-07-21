use serde::Deserialize;

use crate::{define_wrapper_type, input::Input, mode::Mode};

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Case {
    pub name: Option<String>,
    pub comment: Option<String>,
    pub modes: Option<Vec<Mode>>,
    pub inputs: Vec<Input>,
    pub group: Option<String>,
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    CaseIdx(usize);
);
