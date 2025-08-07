//! This module contains a parser for the Solidity semantic tests allowing them to be parsed into
//! regular [`Metadata`] objects that can be executed by the testing framework.
//!
//! [`Metadata`]: crate::metadata::Metadata

mod function_parser;
mod sections;
mod test_configuration;

pub use function_parser::*;
pub use sections::*;
pub use test_configuration::*;
