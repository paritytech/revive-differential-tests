use alloy::json_abi::Function;
use semver::VersionReq;
use serde::{Deserialize, de::Deserializer};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Input {
    pub instance: Option<String>,
    #[serde(deserialize_with = "deserialize_method")]
    pub method: Method,
    pub calldata: Option<Calldata>,
    pub expected: Option<Expected>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Expected {
    Calldata(Calldata),
    Expected(ExpectedOutput),
    ExpectedMany(Vec<ExpectedOutput>),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ExpectedOutput {
    compiler_version: Option<VersionReq>,
    return_data: Option<Calldata>,
    events: Option<Value>,
    exception: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Calldata {
    Single(String),
    Compound(Vec<String>),
}

/// Specify how the contract is called.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub enum Method {
    /// Initiate a deploy transaction, calling contracts constructor.
    ///
    /// Indicated by `#deployer`.
    Deployer,
    /// Does not calculate and insert a function selector.
    ///
    /// Indicated by `#fallback`.
    #[default]
    Fallback,
    /// Call the public function with this selector.
    ///
    /// Calculates the selector if neither deployer or fallback matches.
    Function([u8; 4]),
}

fn deserialize_method<'de, D>(deserializer: D) -> Result<Method, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match String::deserialize(deserializer)?.as_str() {
        "#deployer" => Method::Deployer,
        "#fallback" => Method::Fallback,
        signature => {
            let signature = if signature.ends_with(')') {
                signature.to_string()
            } else {
                format!("{signature}()")
            };
            match Function::parse(&signature) {
                Ok(function) => Method::Function(function.selector().0),
                Err(error) => {
                    return Err(serde::de::Error::custom(format!(
                        "parsing function signature '{signature}' error: {error}"
                    )));
                }
            }
        }
    })
}
