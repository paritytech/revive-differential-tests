//! This module contains all of the code responsible for performing compilations,
//! including the driver implementation and the core logic that allows for contracts
//! to be compiled in pre-link-only mode without any test execution.

mod driver;
mod entry_point;

pub use driver::*;
pub use entry_point::*;
