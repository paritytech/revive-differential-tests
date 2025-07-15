//! The test driver handles the compilation and execution of the test cases.

use alloy::json_abi::JsonAbi;
use alloy::network::TransactionBuilder;
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::GethTrace;
use alloy::{
    primitives::{Address, map::HashMap},
    rpc::types::{
        TransactionRequest,
        trace::geth::{AccountState, DiffMode},
    },
};
use revive_dt_compiler::{Compiler, CompilerInput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::{input::Input, metadata::Metadata, mode::SolcMode};
use revive_dt_node_interaction::EthereumNode;
use revive_dt_report::reporter::{CompilationTask, Report, Span};
use revive_solc_json_interface::SolcStandardJsonOutput;
use serde_json::Value;
use std::collections::HashMap as StdHashMap;
use tracing::Level;

use crate::Platform;

type Contracts<T> = HashMap<
    CompilerInput<<<T as Platform>::Compiler as SolidityCompiler>::Options>,
    SolcStandardJsonOutput,
>;

pub struct State<'a, T: Platform> {
    config: &'a Arguments,
    span: Span,
    contracts: Contracts<T>,
    deployed_contracts: StdHashMap<String, Address>,
    deployed_abis: StdHashMap<String, JsonAbi>,
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
            deployed_abis: Default::default(),
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

        let compiler = Compiler::<T::Compiler>::new()
            .allow_path(metadata.directory()?)
            .solc_optimizer(mode.solc_optimize());

        let compiler = FilesWithExtensionIterator::new(metadata.directory()?)
            .with_allowed_extension("sol")
            .try_fold(compiler, |compiler, path| compiler.with_source(&path))?;

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
                                tracing::debug!(
                                    "Compiled contract: {contract_name} from file: {file}"
                                );
                            }
                        }
                    } else {
                        tracing::warn!("Compiled contracts field is None");
                    }
                }

                Report::compilation(span, T::config_id(), task);
                Ok(())
            }
            Err(error) => {
                tracing::error!("Failed to compile contract: {:?}", error.to_string());
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
        tracing::trace!("Calling execute_input for input: {input:?}");

        let nonce = node.fetch_add_nonce(input.caller)?;

        tracing::debug!(
            "Nonce calculated on the execute contract, calculated nonce {}, for contract {}, having address {} on node: {}",
            &nonce,
            &input.instance,
            &input.caller,
            std::any::type_name::<T>()
        );

        let tx =
            match input.legacy_transaction(nonce, &self.deployed_contracts, &self.deployed_abis) {
                Ok(tx) => {
                    tracing::debug!("Legacy transaction data: {tx:#?}");
                    tx
                }
                Err(err) => {
                    tracing::error!("Failed to construct legacy transaction: {err:?}");
                    return Err(err);
                }
            };

        tracing::trace!("Executing transaction for input: {input:?}");

        let receipt = match node.execute_transaction(tx) {
            Ok(receipt) => receipt,
            Err(err) => {
                tracing::error!(
                    "Failed to execute transaction when executing the contract: {}, {:?}",
                    &input.instance,
                    err
                );
                return Err(err);
            }
        };

        tracing::trace!(
            "Transaction receipt for executed contract: {} - {:?}",
            &input.instance,
            receipt,
        );

        let trace = node.trace_transaction(receipt.clone())?;
        tracing::trace!(
            "Trace result for contract: {} - {:?}",
            &input.instance,
            trace
        );

        let diff = node.state_diff(receipt.clone())?;

        Ok((receipt, trace, diff))
    }

    pub fn deploy_contracts(&mut self, input: &Input, node: &T::Blockchain) -> anyhow::Result<()> {
        let tracing_span = tracing::debug_span!(
            "Deploying contracts",
            ?input,
            node = std::any::type_name::<T>()
        );
        let _guard = tracing_span.enter();

        tracing::debug!(number_of_contracts_to_deploy = self.contracts.len());

        for output in self.contracts.values() {
            let Some(contract_map) = &output.contracts else {
                tracing::debug!(
                    "No contracts in output — skipping deployment for this input {}",
                    &input.instance
                );
                continue;
            };

            for contracts in contract_map.values() {
                for (contract_name, contract) in contracts {
                    let tracing_span = tracing::info_span!("Deploying contract", contract_name);
                    let _guard = tracing_span.enter();

                    tracing::debug!(
                        "Contract name is: {:?} and the input name is: {:?}",
                        &contract_name,
                        &input.instance
                    );

                    let bytecode = contract
                        .evm
                        .as_ref()
                        .and_then(|evm| evm.bytecode.as_ref())
                        .map(|b| b.object.clone());

                    let Some(code) = bytecode else {
                        tracing::error!("no bytecode for contract {contract_name}");
                        continue;
                    };

                    let nonce = node.fetch_add_nonce(input.caller)?;

                    tracing::debug!(
                        "Calculated nonce {}, for contract {}, having address {} on node: {}",
                        &nonce,
                        &input.instance,
                        &input.caller,
                        std::any::type_name::<T>()
                    );

                    // We are using alloy for building and submitting the transactions and it will
                    // automatically fill in all of the missing fields from the provider that we
                    // are using.
                    let code = alloy::hex::decode(&code)?;
                    let tx = TransactionRequest::default()
                        .nonce(nonce)
                        .from(input.caller)
                        .with_deploy_code(code);

                    let receipt = match node.execute_transaction(tx) {
                        Ok(receipt) => receipt,
                        Err(err) => {
                            tracing::error!(
                                "Failed to execute transaction when deploying the contract on node : {:?}, {:?}, {:?}",
                                std::any::type_name::<T>(),
                                &contract_name,
                                err
                            );
                            return Err(err);
                        }
                    };

                    tracing::debug!(
                        "Deployment tx sent for {} with nonce {} → tx hash: {:?}, on node: {:?}",
                        contract_name,
                        nonce,
                        receipt.transaction_hash,
                        std::any::type_name::<T>(),
                    );

                    tracing::trace!(
                        "Deployed transaction receipt for contract: {} - {:?}, on node: {:?}",
                        &contract_name,
                        receipt,
                        std::any::type_name::<T>(),
                    );

                    let Some(address) = receipt.contract_address else {
                        tracing::error!(
                            "contract {contract_name} deployment did not return an address"
                        );
                        continue;
                    };

                    self.deployed_contracts
                        .insert(contract_name.clone(), address);
                    tracing::trace!(
                        "deployed contract `{}` at {:?}, on node {:?}",
                        contract_name,
                        address,
                        std::any::type_name::<T>()
                    );

                    if let Some(Value::String(metadata_json_str)) = &contract.metadata {
                        tracing::trace!(
                            "metadata found for contract {contract_name}, {metadata_json_str}"
                        );

                        match serde_json::from_str::<serde_json::Value>(metadata_json_str) {
                            Ok(metadata_json) => {
                                if let Some(abi_value) =
                                    metadata_json.get("output").and_then(|o| o.get("abi"))
                                {
                                    match serde_json::from_value::<JsonAbi>(abi_value.clone()) {
                                        Ok(parsed_abi) => {
                                            tracing::trace!(
                                                "ABI found in metadata for contract {}",
                                                &contract_name
                                            );
                                            self.deployed_abis
                                                .insert(contract_name.clone(), parsed_abi);
                                        }
                                        Err(err) => {
                                            anyhow::bail!(
                                                "Failed to parse ABI from metadata for contract {}: {}",
                                                contract_name,
                                                err
                                            );
                                        }
                                    }
                                } else {
                                    anyhow::bail!(
                                        "No ABI found in metadata for contract {}",
                                        contract_name
                                    );
                                }
                            }
                            Err(err) => {
                                anyhow::bail!(
                                    "Failed to parse metadata JSON string for contract {}: {}",
                                    contract_name,
                                    err
                                );
                            }
                        }
                    } else {
                        anyhow::bail!("No metadata found for contract {}", contract_name);
                    }
                }
            }
        }

        tracing::debug!("Available contracts: {:?}", self.deployed_contracts.keys());

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
        tracing::trace!("{label} - PRE STATE:");
        for (addr, state) in &diff.pre {
            Self::trace_account_state("  [pre]", addr, state);
        }

        tracing::trace!("{label} - POST STATE:");
        for (addr, state) in &diff.post {
            Self::trace_account_state("  [post]", addr, state);
        }
    }

    fn trace_account_state(prefix: &str, addr: &Address, state: &AccountState) {
        tracing::trace!("{prefix} 0x{addr:x}");

        if let Some(balance) = &state.balance {
            tracing::trace!("{prefix}   balance: {balance}");
        }
        if let Some(nonce) = &state.nonce {
            tracing::trace!("{prefix}   nonce: {nonce}");
        }
        if let Some(code) = &state.code {
            tracing::trace!("{prefix}   code: {code}");
        }
    }

    pub fn execute(&mut self, span: Span) -> anyhow::Result<()> {
        for mode in self.metadata.solc_modes() {
            let mut leader_state = State::<L>::new(self.config, span);
            leader_state.build_contracts(&mode, self.metadata)?;

            let mut follower_state = State::<F>::new(self.config, span);
            follower_state.build_contracts(&mode, self.metadata)?;

            for (case_idx, case) in self.metadata.cases.iter().enumerate() {
                // Creating a tracing span to know which case within the metadata is being executed
                // and which one we're getting logs for.
                let tracing_span = tracing::span!(
                    Level::INFO,
                    "Executing case",
                    case = case.name,
                    case_idx = case_idx
                );
                let _guard = tracing_span.enter();

                for input in &case.inputs {
                    tracing::debug!("Starting deploying contract {}", &input.instance);
                    if let Err(err) = leader_state.deploy_contracts(input, self.leader_node) {
                        tracing::error!("Leader deployment failed for {}: {err}", input.instance);
                        continue;
                    } else {
                        tracing::debug!("Leader deployment succeeded for {}", &input.instance);
                    }

                    if let Err(err) = follower_state.deploy_contracts(input, self.follower_node) {
                        tracing::error!("Follower deployment failed for {}: {err}", input.instance);
                        continue;
                    } else {
                        tracing::debug!("Follower deployment succeeded for {}", &input.instance);
                    }

                    tracing::debug!("Starting executing contract {}", &input.instance);

                    let (leader_receipt, _, leader_diff) =
                        match leader_state.execute_input(input, self.leader_node) {
                            Ok(result) => result,
                            Err(err) => {
                                tracing::error!(
                                    "Leader execution failed for {}: {err}",
                                    input.instance
                                );
                                continue;
                            }
                        };

                    let (follower_receipt, _, follower_diff) =
                        match follower_state.execute_input(input, self.follower_node) {
                            Ok(result) => result,
                            Err(err) => {
                                tracing::error!(
                                    "Follower execution failed for {}: {err}",
                                    input.instance
                                );
                                continue;
                            }
                        };

                    if leader_diff == follower_diff {
                        tracing::debug!("State diffs match between leader and follower.");
                    } else {
                        tracing::debug!("State diffs mismatch between leader and follower.");
                        Self::trace_diff_mode("Leader", &leader_diff);
                        Self::trace_diff_mode("Follower", &follower_diff);
                    }

                    if leader_receipt.logs() != follower_receipt.logs() {
                        tracing::debug!("Log/event mismatch between leader and follower.");
                        tracing::trace!("Leader logs: {:?}", leader_receipt.logs());
                        tracing::trace!("Follower logs: {:?}", follower_receipt.logs());
                    }

                    if leader_receipt.status() != follower_receipt.status() {
                        tracing::debug!(
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

/// An iterator that finds files of a certain extension in the provided directory. You can think of
/// this a glob pattern similar to: `${path}/**/*.md`
struct FilesWithExtensionIterator {
    /// The set of allowed extensions that that match the requirement and that should be returned
    /// when found.
    allowed_extensions: std::collections::HashSet<std::borrow::Cow<'static, str>>,

    /// The set of directories to visit next. This iterator does BFS and so these directories will
    /// only be visited if we can't find any files in our state.
    directories_to_search: Vec<std::path::PathBuf>,

    /// The set of files matching the allowed extensions that were found. If there are entries in
    /// this vector then they will be returned when the [`Iterator::next`] method is called. If not
    /// then we visit one of the next directories to visit.
    ///
    /// [`Iterator`]: std::iter::Iterator
    files_matching_allowed_extensions: Vec<std::path::PathBuf>,
}

impl FilesWithExtensionIterator {
    fn new(root_directory: std::path::PathBuf) -> Self {
        Self {
            allowed_extensions: Default::default(),
            directories_to_search: vec![root_directory],
            files_matching_allowed_extensions: Default::default(),
        }
    }

    fn with_allowed_extension(
        mut self,
        allowed_extension: impl Into<std::borrow::Cow<'static, str>>,
    ) -> Self {
        self.allowed_extensions.insert(allowed_extension.into());
        self
    }
}

impl Iterator for FilesWithExtensionIterator {
    type Item = std::path::PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(file_path) = self.files_matching_allowed_extensions.pop() {
            return Some(file_path);
        };

        let directory_to_search = self.directories_to_search.pop()?;

        // Read all of the entries in the directory. If we failed to read this dir's entires then we
        // elect to just ignore it and look in the next directory, we do that by calling the next
        // method again on the iterator, which is an intentional decision that we made here instead
        // of panicking.
        let Ok(dir_entries) = std::fs::read_dir(directory_to_search) else {
            return self.next();
        };

        for entry in dir_entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                self.directories_to_search.push(entry_path)
            } else if entry_path.is_file()
                && entry_path.extension().is_some_and(|ext| {
                    self.allowed_extensions
                        .iter()
                        .any(|allowed| ext.eq_ignore_ascii_case(allowed.as_ref()))
                })
            {
                self.files_matching_allowed_extensions.push(entry_path)
            }
        }

        self.next()
    }
}
