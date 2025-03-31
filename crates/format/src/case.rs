use serde::Deserialize;

use crate::{input::Input, mode::Mode};

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Case {
    pub name: Option<String>,
    pub comment: Option<String>,
    pub modes: Option<Vec<Mode>>,
    pub inputs: Vec<Input>,
    pub group: Option<String>,
}
