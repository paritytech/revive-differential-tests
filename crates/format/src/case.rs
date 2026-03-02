use alloy::primitives::{Address, map::HashSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use revive_dt_common::{
    macros::define_wrapper_type,
    types::{Mode, ParsedMode, VmIdentifier},
};

use crate::steps::*;

#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq, JsonSchema)]
pub struct Case {
    /// An optional vector of targets that this Metadata file's cases can be executed on. As an
    /// example, if we wish for the metadata file's cases to only be run on PolkaVM then we'd
    /// specify a target of "PolkaVM" in here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<HashSet<VmIdentifier>>,

    /// An optional name of the test case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// An optional comment on the case which has no impact on the execution in any way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// This represents a mode that has been parsed from test metadata.
    ///
    /// Mode strings can take the following form (in pseudo-regex):
    ///
    /// ```text
    /// [YEILV][+-]? (M[0123sz])? <semver>?
    /// ```
    ///
    /// If this is provided then it takes higher priority than the modes specified in the metadata
    /// file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<ParsedMode>>,

    /// The set of steps to run as part of this test case.
    #[serde(rename = "inputs")]
    pub steps: Vec<Step>,

    /// An optional name of the group of tests that this test belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    /// An optional set of expectations and assertions to make about the transaction after it ran.
    ///
    /// If this is not specified then the only assertion that will be ran is that the transaction
    /// was successful.
    ///
    /// This expectation that's on the case itself will be attached to the final step of the case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Expected>,

    /// An optional boolean which defines if the case as a whole should be ignored. If null then the
    /// case will not be ignored.
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

    pub fn steps_iterator_for_benchmarks(
        &self,
        default_repeat_count: usize,
    ) -> Box<dyn Iterator<Item = Step> + '_> {
        let contains_repeat = self
            .steps_iterator()
            .any(|step| matches!(&step, Step::Repeat(..)));
        if contains_repeat {
            Box::new(self.steps_iterator()) as Box<_>
        } else {
            Box::new(std::iter::once(Step::Repeat(Box::new(RepeatStep {
                comment: None,
                repeat: default_repeat_count,
                steps: self.steps_iterator().collect(),
            })))) as Box<_>
        }
    }

    pub fn compiler_modes(&self) -> Vec<Mode> {
        match &self.modes {
            Some(modes) => ParsedMode::many_to_modes(modes.iter()).collect(),
            None => Mode::all().cloned().collect(),
        }
    }

    pub fn deployer_address(&self) -> Address {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Step::FunctionCall(input) => input.caller.as_address().copied(),
                Step::BalanceAssertion(..) => None,
                Step::StorageEmptyAssertion(..) => None,
                Step::Repeat(..) => None,
                Step::AllocateAccount(..) => None,
            })
            .next()
            .unwrap_or(FunctionCallStep::default_caller_address())
    }

    pub fn any_step_expected_to_fail(&self) -> bool {
        self.steps_iterator().any(|step| {
            let Step::FunctionCall(function_call_step) = step else {
                return false;
            };
            let FunctionCallStep {
                expected: Some(expected),
                ..
            } = *function_call_step
            else {
                return false;
            };
            match expected {
                Expected::Calldata(..) => false,
                Expected::Expected(expected_output) => expected_output.exception,
                Expected::ExpectedMany(expected_outputs) => {
                    expected_outputs.iter().any(|value| value.exception)
                }
            }
        })
    }
}

define_wrapper_type!(
    /// A wrapper type for the index of test cases found in metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct CaseIdx(usize) impl Display, FromStr;
);
