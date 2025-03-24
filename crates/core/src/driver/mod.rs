//! The test driver handles the compilation and execution of the test cases.

use alloy::primitives::map::HashMap;
use compiler::build;
use revive_dt_compiler::{CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::metadata::Metadata;
use revive_dt_node::Node;
use revive_solc_json_interface::SolcStandardJsonOutput;

use crate::Platform;

pub mod compiler;
pub mod input;

type Contracts<T> = HashMap<
    CompilerInput<<<T as Platform>::Compiler as SolidityCompiler>::Options>,
    SolcStandardJsonOutput,
>;

pub struct Driver<'a, Leader: Platform, Follower: Platform> {
    metadata: &'a Metadata,
    config: &'a Arguments,

    leader_contracts: Contracts<Leader>,
    leader_node: <Leader as Platform>::Blockchain,

    follower_contracts: Contracts<Follower>,
    follower_node: <Follower as Platform>::Blockchain,
}

impl<'a, L, F> Driver<'a, L, F>
where
    L: Platform + Default,
    F: Platform + Default,
{
    pub fn new(metadata: &'a Metadata, config: &'a Arguments) -> Driver<'a, L, F> {
        Self {
            metadata,
            config,

            leader_node: <<L as Platform>::Blockchain as Node>::new(config),
            leader_contracts: Default::default(),

            follower_node: <<F as Platform>::Blockchain as Node>::new(config),
            follower_contracts: Default::default(),
        }
    }

    pub fn execute(&mut self) -> anyhow::Result<()> {
        self.leader_contracts = build::<L::Compiler>(self.metadata)?;
        self.follower_contracts = build::<F::Compiler>(self.metadata)?;

        if self.config.compile_only {
            return Ok(());
        }

        todo!()
    }
}
