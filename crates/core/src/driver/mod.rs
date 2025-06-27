//! The test driver handles the compilation and execution of the test cases.

use alloy::primitives::Bytes;
use alloy::rpc::types::TransactionInput;
use alloy::{
    primitives::{Address, TxKind, map::HashMap},
    rpc::types::{
        TransactionReceipt, TransactionRequest,
        trace::geth::{AccountState, DiffMode, GethTrace},
    },
};
use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::{input::Input, metadata::Metadata, mode::SolcMode};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::reporter::{CompilationTask, Report, Span};
use revive_solc_json_interface::SolcStandardJsonOutput;

use crate::Platform;

type Contracts<T> = HashMap<
    CompilerInput<<<T as Platform>::Compiler as SolidityCompiler>::Options>,
    SolcStandardJsonOutput,
>;

pub struct State<'a, T: Platform> {
    config: &'a Arguments,
    span: Span,
    contracts: Contracts<T>,
    deployed_contracts: HashMap<String, Address>,
}

impl<'a, T> State<'a, T>
where
    T: Platform,
{
    pub fn new(config: &'a Arguments, span: Span) -> Self {
        Self {
            config,
            span,
            contracts: Default::default(),
            deployed_contracts: Default::default(),
        }
    }

    /// Returns a copy of the current span.
    fn span(&self) -> Span {
        self.span
    }

    pub fn build_contracts(&mut self, mode: &SolcMode, metadata: &Metadata) -> anyhow::Result<()> {
        let mut span = self.span();
        span.next_metadata(
            metadata
                .file_path
                .as_ref()
                .expect("metadata should have been read from a file")
                .clone(),
        );

        let Some(version) = mode.last_patch_version(&self.config.solc) else {
            anyhow::bail!("unsupported solc version: {:?}", &mode.solc_version);
        };

        let mut compiler = Compiler::<T::Compiler>::new()
            .base_path(metadata.directory()?.display().to_string())
            .solc_optimizer(mode.solc_optimize());

        for (file, _contract) in metadata.contract_sources()?.values() {
            log::debug!("contract source {}", file.display());
            compiler = compiler.with_source(file)?;
        }

        let mut task = CompilationTask {
            json_input: compiler.input(),
            json_output: None,
            mode: mode.clone(),
            compiler_version: format!("{}", &version),
            error: None,
        };

        let compiler_path = T::Compiler::get_compiler_executable(self.config, version)?;
        match compiler.try_build(compiler_path) {
            Ok(output) => {
                task.json_output = Some(output.output.clone());
                task.error = output.error;
                self.contracts.insert(output.input, output.output);

                if let Some(last_output) = self.contracts.values().last() {
                    if let Some(contracts) = &last_output.contracts {
                        for (file, contracts_map) in contracts {
                            for contract_name in contracts_map.keys() {
                                log::debug!(
                                    "Compiled contract: {} from file: {}",
                                    contract_name,
                                    file
                                );
                            }
                        }
                    } else {
                        log::warn!("Compiled contracts field is None");
                    }
                }

                Report::compilation(span, T::config_id(), task);
                Ok(())
            }
            Err(error) => {
                log::error!("Failed to compile contract: {:?}", error.to_string());
                task.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    pub fn execute_input(
        &mut self,
        input: &Input,
        node: &T::Blockchain,
    ) -> anyhow::Result<(TransactionReceipt, GethTrace, DiffMode)> {
        log::trace!("Calling execute_input for input: {:?}", input);

        let nonce = node.fetch_add_nonce(input.caller)?;

        log::debug!(
            "Nonce calculated on the execute contract, calculated nonce {}, for contract {}, having address {} on node: {}",
            &nonce,
            &input.instance,
            &input.caller,
            std::any::type_name::<T>()
        );

        let tx =
            match input.legacy_transaction(self.config.network_id, nonce, &self.deployed_contracts)
            {
                Ok(tx) => tx,
                Err(err) => {
                    log::error!("Failed to construct legacy transaction: {:?}", err);
                    return Err(err);
                }
            };

        log::trace!("Executing transaction for input: {:?}", input);

        let receipt = match node.execute_transaction(tx) {
            Ok(receipt) => receipt,
            Err(err) => {
                log::error!(
                    "Failed to execute transaction when executing the contract: {}, {:?}",
                    &input.instance,
                    err
                );
                return Err(err);
            }
        };

        log::trace!(
            "Transaction receipt for executed contract: {} - {:?}",
            &input.instance,
            receipt,
        );

        let trace = node.trace_transaction(receipt.clone())?;
        log::trace!(
            "Trace result for contract: {} - {:?}",
            &input.instance,
            trace
        );

        let diff = node.state_diff(receipt.clone())?;

        Ok((receipt, trace, diff))
    }

    pub fn deploy_contracts(&mut self, input: &Input, node: &T::Blockchain) -> anyhow::Result<()> {
        log::debug!(
            "Deploying contracts {}, having address {} on node: {}",
            &input.instance,
            &input.caller,
            std::any::type_name::<T>()
        );
        for output in self.contracts.values() {
            let Some(contract_map) = &output.contracts else {
                log::debug!(
                    "No contracts in output — skipping deployment for this input {}",
                    &input.instance
                );
                continue;
            };

            for contracts in contract_map.values() {
                for (contract_name, contract) in contracts {
                    log::debug!(
                        "Contract name is: {:?} and the input name is: {:?}",
                        &contract_name,
                        &input.instance
                    );
                    if contract_name != &input.instance {
                        continue;
                    }

                    let bytecode = contract
                        .evm
                        .as_ref()
                        .and_then(|evm| evm.bytecode.as_ref())
                        .map(|b| b.object.clone());

                    let Some(code) = bytecode else {
                        log::error!("no bytecode for contract {}", contract_name);
                        continue;
                    };

                    let nonce = node.fetch_add_nonce(input.caller)?;

                    log::debug!(
                        "Calculated nonce {}, for contract {}, having address {} on node: {}",
                        &nonce,
                        &input.instance,
                        &input.caller,
                        std::any::type_name::<T>()
                    );

                    let tx = TransactionRequest {
                        from: Some(input.caller),
                        to: Some(TxKind::Create),
                        gas_price: Some(5_000_000),
                        gas: Some(5_000_000),
                        chain_id: Some(self.config.network_id),
                        nonce: Some(nonce),
                        input: TransactionInput::new(Bytes::from(code.into_bytes())),
                        ..Default::default()
                    };

                    let receipt = match node.execute_transaction(tx) {
                        Ok(receipt) => receipt,
                        Err(err) => {
                            log::error!(
                                "Failed to execute transaction when deploying the contract on node : {:?}, {:?}, {:?}",
                                std::any::type_name::<T>(),
                                &contract_name,
                                err
                            );
                            return Err(err);
                        }
                    };

                    log::info!(
                        "Deployment tx sent for {} with nonce {} → tx hash: {:?}, on node: {:?}",
                        contract_name,
                        nonce,
                        receipt.transaction_hash,
                        std::any::type_name::<T>(),
                    );

                    log::trace!(
                        "Deployed transaction receipt for contract: {} - {:?}, on node: {:?}",
                        &contract_name,
                        receipt,
                        std::any::type_name::<T>(),
                    );

                    let Some(address) = receipt.contract_address else {
                        log::error!(
                            "contract {} deployment did not return an address",
                            contract_name
                        );
                        continue;
                    };

                    self.deployed_contracts
                        .insert(contract_name.clone(), address);
                    log::info!(
                        "deployed contract `{}` at {:?}, on node {:?}",
                        contract_name,
                        address,
                        std::any::type_name::<T>()
                    );
                }
            }
        }

        log::debug!("Available contracts: {:?}", self.deployed_contracts.keys());

        Ok(())
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

    pub fn trace_diff_mode(label: &str, diff: &DiffMode) {
        log::trace!("{} - PRE STATE:", label);
        for (addr, state) in &diff.pre {
            Self::trace_account_state("  [pre]", addr, state);
        }

        log::trace!("{} - POST STATE:", label);
        for (addr, state) in &diff.post {
            Self::trace_account_state("  [post]", addr, state);
        }
    }

    fn trace_account_state(prefix: &str, addr: &Address, state: &AccountState) {
        log::trace!("{} 0x{:x}", prefix, addr);

        if let Some(balance) = &state.balance {
            log::trace!("{}   balance: {}", prefix, balance);
        }
        if let Some(nonce) = &state.nonce {
            log::trace!("{}   nonce: {}", prefix, nonce);
        }
        if let Some(code) = &state.code {
            log::trace!("{}   code: {}", prefix, code);
        }
    }

    pub fn execute(&mut self, span: Span) -> anyhow::Result<()> {
        for mode in self.metadata.solc_modes() {
            let mut leader_state = State::<L>::new(self.config, span);
            leader_state.build_contracts(&mode, self.metadata)?;

            let mut follower_state = State::<F>::new(self.config, span);
            follower_state.build_contracts(&mode, self.metadata)?;

            for case in &self.metadata.cases {
                for input in &case.inputs {
                    log::debug!("Starting deploying contract {}", &input.instance);
                    leader_state.deploy_contracts(input, self.leader_node)?;
                    follower_state.deploy_contracts(input, self.follower_node)?;

                    log::debug!("Starting executing contract {}", &input.instance);
                    let (leader_receipt, _, leader_diff) =
                        leader_state.execute_input(input, self.leader_node)?;
                    let (follower_receipt, _, follower_diff) =
                        follower_state.execute_input(input, self.follower_node)?;

                    if leader_diff == follower_diff {
                        log::debug!("State diffs match between leader and follower.");
                    } else {
                        log::debug!("State diffs mismatch between leader and follower.");
                        Self::trace_diff_mode("Leader", &leader_diff);
                        Self::trace_diff_mode("Follower", &follower_diff);
                    }

                    if leader_receipt.logs() != follower_receipt.logs() {
                        log::debug!("Log/event mismatch between leader and follower.");
                        log::trace!("Leader logs: {:?}", leader_receipt.logs());
                        log::trace!("Follower logs: {:?}", follower_receipt.logs());
                    }

                    if leader_receipt.status() != follower_receipt.status() {
                        log::debug!(
                            "Mismatch in status: leader = {}, follower = {}",
                            leader_receipt.status(),
                            follower_receipt.status()
                        );
                    }
                }
            }
        }

        Ok(())
    }
}
