use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{case::Case, mode::Mode};

#[derive(Debug, Default, Deserialize, Clone, Eq, PartialEq)]
pub struct Metadata {
    pub cases: Vec<Case>,
    pub contracts: Option<BTreeMap<String, String>>,
    pub libraries: Option<BTreeMap<String, BTreeMap<String, String>>>,
    pub ignore: Option<bool>,
    pub modes: Option<Vec<Mode>>,
}
