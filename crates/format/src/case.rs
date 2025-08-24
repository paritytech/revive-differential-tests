use serde::{Deserialize, Serialize};

use revive_dt_common::{macros::define_wrapper_type, types::Mode};

use crate::{
    input::{Expected, Step},
    mode::ParsedMode,
};

#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct Case {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<ParsedMode>>,

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
    #[allow(irrefutable_let_patterns)]
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

    pub fn solc_modes(&self) -> Vec<Mode> {
        match &self.modes {
            Some(modes) => ParsedMode::many_to_modes(modes.iter()).collect(),
            None => Mode::all().collect(),
        }
    }
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct CaseIdx(usize) impl Display, FromStr;
);
