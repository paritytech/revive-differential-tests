//! The test driver handles the compilation and execution of the test cases.

use alloy::primitives::{Address, map::HashMap};
use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::{
    metadata::Metadata,
    mode::{Mode, SolcMode},
};
use revive_dt_node::Node;
use revive_solc_json_interface::SolcStandardJsonOutput;
use semver::Version;

use crate::Platform;

type Contracts<T> = HashMap<
    CompilerInput<<<T as Platform>::Compiler as SolidityCompiler>::Options>,
    SolcStandardJsonOutput,
>;

pub struct State<T: Platform> {
    contracts: Contracts<T>,
    deployed_contracts: HashMap<String, Address>,
    node: T::Blockchain,
}

impl<T> State<T>
where
    T: Platform,
{
    fn new(config: &Arguments) -> Self {
        Self {
            contracts: Default::default(),
            deployed_contracts: Default::default(),
            node: <T::Blockchain as Node>::new(config),
        }
    }

    pub fn build_contracts(&mut self, metadata: &Metadata) -> anyhow::Result<()> {
        let sources = metadata.contract_sources()?;
        let base_path = metadata.directory()?.display().to_string();
        let modes = metadata
            .modes
            .to_owned()
            .unwrap_or_else(|| vec![Mode::Solidity(Default::default())]);

        let mut result = HashMap::new();
        for mode in modes {
            let mut compiler = Compiler::<T::Compiler>::new().base_path(base_path.clone());
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

        Ok(())
    }
}

pub struct Driver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    config: &'a Arguments,
    leader: State<Leader>,
    follower: State<Follower>,
}

impl<'a, L, F> Driver<'a, L, F>
where
    L: Platform,
    F: Platform,
{
    pub fn new(metadata: &'a Metadata, config: &'a Arguments) -> Driver<'a, L, F> {
        Self {
            metadata,
            config,
            leader: State::new(config),
            follower: State::new(config),
        }
    }

    pub fn execute(&mut self) -> anyhow::Result<()> {
        self.leader.build_contracts(self.metadata)?;
        self.follower.build_contracts(self.metadata)?;

        if self.config.compile_only {
            return Ok(());
        }

        todo!()
    }
}
