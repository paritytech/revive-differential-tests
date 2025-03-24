//! The compiler driver builds the test case contracts for selected compilers.

use std::collections::HashMap;

use revive_dt_compiler::{Compiler, solc::Solc};
use revive_dt_format::{
    metadata::Metadata,
    mode::{Mode, SolcMode},
};
use revive_solc_json_interface::SolcStandardJsonOutput;
use semver::Version;

#[derive(Hash, Eq, PartialEq)]
pub struct SolcSettings {
    pub optimizer: bool,
    pub solc_version: Version,
}

pub fn build_evm(
    metadata: &Metadata,
) -> anyhow::Result<HashMap<SolcSettings, SolcStandardJsonOutput>> {
    let sources = metadata.contract_sources()?;
    let base_path = metadata.directory()?.display().to_string();
    let modes = metadata
        .modes
        .to_owned()
        .unwrap_or_else(|| vec![Mode::Solidity(Default::default())]);

    let mut result = HashMap::new();
    for mode in modes {
        let mut compiler = Compiler::<Solc>::new().base_path(base_path.clone());
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
                let out = compiler.solc_optimizer(optimizer).try_build(&version)?;

                result.insert(
                    SolcSettings {
                        optimizer: true,
                        solc_version: version,
                    },
                    out,
                );
            }
            Mode::Unknown(mode) => log::debug!("compiler: ignoring unknown mode '{mode}'"),
        }
    }

    Ok(result)
}
