//! The compiler driver builds the test case contracts for selected compilers.

use std::collections::HashMap;

use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_format::{
    metadata::Metadata,
    mode::{Mode, SolcMode},
};
use revive_solc_json_interface::SolcStandardJsonOutput;
use semver::Version;

pub fn build<T: SolidityCompiler>(
    metadata: &Metadata,
) -> anyhow::Result<HashMap<CompilerInput<T::Options>, SolcStandardJsonOutput>> {
    let sources = metadata.contract_sources()?;
    let base_path = metadata.directory()?.display().to_string();
    let modes = metadata
        .modes
        .to_owned()
        .unwrap_or_else(|| vec![Mode::Solidity(Default::default())]);

    let mut result = HashMap::new();
    for mode in modes {
        let mut compiler = Compiler::<T>::new().base_path(base_path.clone());
        for (file, _contract) in sources.values() {
            compiler = compiler.with_source(file)?;
        }

        match mode {
            Mode::Solidity(SolcMode {
                solc_version: _,
                solc_optimize,
                llvm_optimizer_settings: _,
            }) => {
                let optimizer = solc_optimize.unwrap_or(true);
                let version = Version::new(0, 8, 29);
                let output = compiler.solc_optimizer(optimizer).try_build(&version)?;
                result.insert(output.input, output.output);
            }
            Mode::Unknown(mode) => log::debug!("compiler: ignoring unknown mode '{mode}'"),
        }
    }

    Ok(result)
}
