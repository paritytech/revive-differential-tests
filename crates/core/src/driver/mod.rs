//! The test driver handles the compilation and execution of the test cases.

use alloy::{
    primitives::{Address, map::HashMap},
    rpc::types::trace::geth::GethTrace,
};
use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::{
    input::Input,
    metadata::Metadata,
    mode::{Mode, SolcMode},
};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_solc_binaries::download_solc;
use revive_solc_json_interface::SolcStandardJsonOutput;

use crate::Platform;

type Contracts<T> = HashMap<
    CompilerInput<<<T as Platform>::Compiler as SolidityCompiler>::Options>,
    SolcStandardJsonOutput,
>;

pub struct State<'a, T: Platform> {
    config: &'a Arguments,
    contracts: Contracts<T>,
    deployed_contracts: HashMap<String, Address>,
}

impl<'a, T> State<'a, T>
where
    T: Platform,
{
    fn new(config: &'a Arguments) -> Self {
        Self {
            config,
            contracts: Default::default(),
            deployed_contracts: Default::default(),
        }
    }

    pub fn build_contracts(&mut self, mode: &SolcMode, metadata: &Metadata) -> anyhow::Result<()> {
        let Some(version) = mode.last_patch_version(&self.config.solc) else {
            anyhow::bail!("unsupported solc version: {:?}", mode.solc_version);
        };

        let sources = metadata.contract_sources()?;
        let base_path = metadata.directory()?.display().to_string();
        let mut compiler = Compiler::<T::Compiler>::new().base_path(base_path.clone());
        for (file, _contract) in sources.values() {
            compiler = compiler.with_source(file)?;
        }

        let solc_path = download_solc(self.config.directory(), version, self.config.wasm)?;
        let output = compiler
            .solc_optimizer(mode.solc_optimize())
            .try_build(solc_path)?;

        self.contracts.insert(output.input, output.output);

        Ok(())
    }

    pub fn execute_input(
        &mut self,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<GethTrace> {
        let receipt = node.execute_transaction(input.legacy_transaction(
            self.config.network_id,
            0,
            &self.deployed_contracts,
        )?)?;
        dbg!(&receipt);
        Ok(node.trace_transaction(receipt)?)
    }
}

pub struct Driver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    config: &'a Arguments,
    leader: State<'a, Leader>,
    follower: State<'a, Follower>,
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

    pub fn execute(
        &mut self,
        leader: L::Blockchain,
        follower: F::Blockchain,
    ) -> anyhow::Result<()> {
        for mode in self.modes() {
            self.leader.build_contracts(&mode, self.metadata)?;
            self.follower.build_contracts(&mode, self.metadata)?;

            if self.config.compile_only {
                continue;
            }

            for case in &self.metadata.cases {
                for input in &case.inputs {
                    let expected = self.leader.execute_input(input, &leader)?;
                }
            }

            *self = Self::new(self.metadata, self.config);
        }

        Ok(())
    }

    fn modes(&self) -> Vec<SolcMode> {
        self.metadata
            .modes()
            .iter()
            .filter_map(|mode| match mode {
                Mode::Solidity(solc_mode) => Some(solc_mode),
                Mode::Unknown(mode) => {
                    log::debug!("compiler: ignoring unknown mode '{mode}'");
                    None
                }
            })
            .cloned()
            .collect()
    }
}
