use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use revive_dt_common::macros::define_wrapper_type;

use crate::{
    input::{Expected, Step},
    metadata::{deserialize_compilation_modes, serialize_compilation_modes},
    mode::Mode,
};

#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct Case {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_compilation_modes",
        serialize_with = "serialize_compilation_modes"
    )]
    pub modes: Option<HashSet<Mode>>,

    #[serde(rename = "inputs")]
    pub steps: Vec<Step>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Expected>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<bool>,
}

impl Case {
    pub fn steps_iterator(&self) -> impl Iterator<Item = Step> {
        let steps_len = self.steps.len();
        self.steps
            .clone()
            .into_iter()
            .enumerate()
            .map(move |(idx, mut step)| {
                let Step::FunctionCall(ref mut input) = step else {
                    return step;
                };

                if idx + 1 == steps_len {
                    if input.expected.is_none() {
                        input.expected = self.expected.clone();
                    }

                    // TODO: What does it mean for us to have an `expected` field on the case itself
                    // but the final input also has an expected field that doesn't match the one on
                    // the case? What are we supposed to do with that final expected field on the
                    // case?

                    step
                } else {
                    step
                }
            })
    }
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct CaseIdx(usize);
);
