//! The test driver handles the compilation and execution of the test cases.

use alloy::{
    primitives::{Address, map::HashMap},
    rpc::types::trace::geth::GethTrace,
};
use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::{input::Input, metadata::Metadata, mode::SolcMode};
use revive_dt_node_interaction::EthereumNode;
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
    pub fn new(config: &'a Arguments) -> Self {
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
            log::debug!("contract source {}", file.display());
            compiler = compiler.with_source(file)?;
        }

        let compiler_path = T::get_compiler_executable(self.config, version)?;

        let output = compiler
            .solc_optimizer(mode.solc_optimize())
            .try_build(compiler_path)?;

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
        //node.trace_transaction(receipt)
        todo!()
    }
}

pub struct Driver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    config: &'a Arguments,
    leader_node: &'a Leader::Blockchain,
    follower_node: &'a Follower::Blockchain,
}

impl<'a, L, F> Driver<'a, L, F>
where
    L: Platform,
    F: Platform,
{
    pub fn new(
        metadata: &'a Metadata,
        config: &'a Arguments,
        leader_node: &'a L::Blockchain,
        follower_node: &'a F::Blockchain,
    ) -> Driver<'a, L, F> {
        Self {
            metadata,
            config,
            leader_node,
            follower_node,
        }
    }

    pub fn execute(&mut self) -> anyhow::Result<()> {
        for mode in self.metadata.solc_modes() {
            let mut leader_state = State::<L>::new(self.config);
            leader_state.build_contracts(&mode, self.metadata)?;

            let mut follower_state = State::<F>::new(self.config);
            follower_state.build_contracts(&mode, self.metadata)?;

            for case in &self.metadata.cases {
                for input in &case.inputs {
                    let _ = leader_state.execute_input(input, self.leader_node)?;
                    let _ = follower_state.execute_input(input, self.follower_node)?;
                }
            }
        }

        Ok(())
    }
}
