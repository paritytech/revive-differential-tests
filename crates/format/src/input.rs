use alloy::json_abi::Function;
use alloy::primitives::U256;
use serde::{Deserialize, de::Deserializer};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Input {
    pub instance: String,
    #[serde(deserialize_with = "deserialize_method")]
    pub method: Method,
    #[serde(deserialize_with = "deserialize_calldata")]
    pub calldata: Vec<u8>,
    pub expected: Option<Vec<String>>,
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

fn deserialize_calldata<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let calldata_strings: Vec<String> = Vec::deserialize(deserializer)?;
    let mut result = Vec::with_capacity(calldata_strings.len() * 32);

    for calldata_string in &calldata_strings {
        match calldata_string.parse::<U256>() {
            Ok(parsed) => result.extend_from_slice(&parsed.to_be_bytes::<32>()),
            Err(error) => {
                return Err(serde::de::Error::custom(format!(
                    "parsing U256 {calldata_string} error: {error}"
                )));
            }
        };
    }

    Ok(result)
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
