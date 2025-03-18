use serde::Deserialize;

use crate::modes::Mode;

#[derive(Debug, Deserialize)]
pub struct Case {
    #[serde(rename(deserialize = "modes"))]
    pub modes: Vec<Mode>,
}
