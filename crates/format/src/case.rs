use serde::Deserialize;

use crate::{
    define_wrapper_type,
    input::{Expected, Input},
    mode::Mode,
};

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Case {
    pub name: Option<String>,
    pub comment: Option<String>,
    pub modes: Option<Vec<Mode>>,
    pub inputs: Vec<Input>,
    pub group: Option<String>,
    pub expected: Option<Expected>,
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    CaseIdx(usize);
);

impl Case {
    pub fn inputs_iterator(&self) -> impl Iterator<Item = Input> {
        let inputs_len = self.inputs.len();
        self.inputs
            .clone()
            .into_iter()
            .enumerate()
            .map(move |(idx, mut input)| {
                if idx + 1 == inputs_len {
                    input.expected = self.expected.clone();
                    input
                } else {
                    input
                }
            })
    }
}
