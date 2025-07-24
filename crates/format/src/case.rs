use serde::Deserialize;

use revive_dt_common::macros::define_wrapper_type;

use crate::{
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

impl Case {
    pub fn inputs_iterator(&self) -> impl Iterator<Item = Input> {
        let inputs_len = self.inputs.len();
        self.inputs
            .clone()
            .into_iter()
            .enumerate()
            .map(move |(idx, mut input)| {
                if idx + 1 == inputs_len {
                    if input.expected.is_none() {
                        input.expected = self.expected.clone();
                    }

                    // TODO: What does it mean for us to have an `expected` field on the case itself
                    // but the final input also has an expected field that doesn't match the one on
                    // the case? What are we supposed to do with that final expected field on the
                    // case?

                    input
                } else {
                    input
                }
            })
    }
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct CaseIdx(usize);
);
