//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use revive_solc_json_interface::{SolcStandardJsonInput, SolcStandardJsonOutput};
use semver::Version;

use crate::SolidityCompiler;

pub struct Solc {}

impl SolidityCompiler for Solc {
    type Options = ();

    fn build(
        &self,
        _input: &SolcStandardJsonInput,
        _extra_options: &Option<Self::Options>,
    ) -> anyhow::Result<SolcStandardJsonOutput> {
        todo!()
    }

    fn new(_solc_version: &Version) -> Self {
        todo!()
    }
}
