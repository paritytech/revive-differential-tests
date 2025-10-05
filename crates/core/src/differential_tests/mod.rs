//! This module contains all of the code responsible for performing differential tests including the
//! driver implementation, state implementation, and the core logic that allows for tests to be
//! executed.

mod driver;
mod entry_point;
mod execution_state;

pub use driver::*;
pub use entry_point::*;
pub use execution_state::*;
