use serde::Deserialize;

use crate::{
    define_wrapper_type,
    input::{Expected, Input},
    metadata::AddressReplacementMap,
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
    pub fn handle_address_replacement(
        &mut self,
        old_to_new_mapping: &mut AddressReplacementMap,
    ) -> anyhow::Result<()> {
        for input in self.inputs.iter_mut() {
            input.handle_address_replacement(old_to_new_mapping)?;
        }
        if let Some(ref mut expected) = self.expected {
            expected.handle_address_replacement(old_to_new_mapping)?;
        }
        Ok(())
    }
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
