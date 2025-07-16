//! The test driver handles the compilation and execution of the test cases.

use alloy::json_abi::JsonAbi;
use alloy::network::{Ethereum, TransactionBuilder};
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
use std::fmt::Debug;

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

        let tx = match input.legacy_transaction(
            nonce,
            &self.deployed_contracts,
            &self.deployed_abis,
            node,
        ) {
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
                    let tx = {
                        let tx = TransactionRequest::default()
                            .nonce(nonce)
                            .from(input.caller);
                        TransactionBuilder::<Ethereum>::with_deploy_code(tx, code)
                    };

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

                    let Some(Value::String(metadata)) = &contract.metadata else {
                        tracing::error!(?contract, "Contract does not have a metadata field");
                        anyhow::bail!("Contract does not have a metadata field: {contract:?}");
                    };

                    // Deserialize the solc metadata into a JSON object so we can get the ABI of the
                    // contracts. If we fail to perform the deserialization then we return an error
                    // as there's no other way to handle this.
                    let Ok(metadata) = serde_json::from_str::<Value>(metadata) else {
                        tracing::error!(%metadata, "Failed to parse solc metadata into a structured value");
                        anyhow::bail!(
                            "Failed to parse solc metadata into a structured value {metadata}"
                        );
                    };

                    // Accessing the ABI on the solc metadata and erroring if the accessing failed
                    let Some(abi) = metadata.get("output").and_then(|value| value.get("abi"))
                    else {
                        tracing::error!(%metadata, "Failed to access the .output.abi field of the solc metadata");
                        anyhow::bail!(
                            "Failed to access the .output.abi field of the solc metadata {metadata}"
                        );
                    };

                    // Deserialize the ABI object that we got from the unstructured JSON into a
                    // structured ABI object and error out if we fail.
                    let Ok(abi) = serde_json::from_value::<JsonAbi>(abi.clone()) else {
                        tracing::error!(%metadata, "Failed to deserialize ABI into a structured format");
                        anyhow::bail!(
                            "Failed to deserialize ABI into a structured format {metadata}"
                        );
                    };

                    self.deployed_abis.insert(contract_name.clone(), abi);
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

    // A note on this function and the choice of how we handle errors that happen here. This is not
    // a doc comment since it's a comment for the maintainers of this code and not for the users of
    // this code.
    //
    // This function does a few things: it builds the contracts for the various SOLC modes needed.
    // It deploys the contracts to the chain, and it executes the various inputs that are specified
    // for the test cases.
    //
    // In most functions in the codebase, it's fine to just say "If we encounter an error just
    // bubble it up to the caller", but this isn't a good idea to do here and we need an elaborate
    // way to report errors all while being graceful and continuing execution where we can. For
    // example, if one of the inputs of one of the cases fail to execute, then we should not just
    // bubble that error up immediately. Instead, we should note it down and continue to the next
    // case as the next case might succeed.
    //
    // Therefore, this method returns an `ExecutionResult` object, and not just a normal `Result`.
    // This object is fully typed to contain information about what exactly in the execution was a
    // success and what failed.
    //
    // The above then allows us to have better logging and better information in the caller of this
    // function as we have a more detailed view of what worked and what didn't.
    pub fn execute(&mut self, span: Span) -> ExecutionResult {
        // This is the execution result object that all of the execution information will be
        // collected into and returned at the end of the execution.
        let mut execution_result = ExecutionResult::default();

        let tracing_span = tracing::info_span!("Handling metadata file");
        let _guard = tracing_span.enter();

        for mode in self.metadata.solc_modes() {
            let tracing_span = tracing::info_span!("With solc mode", solc_mode = ?mode);
            let _guard = tracing_span.enter();

            let mut leader_state = State::<L>::new(self.config, span);
            let mut follower_state = State::<F>::new(self.config, span);

            // We build the contracts. If building the contracts for the metadata file fails then we
            // have no other option but to keep note of this error and move on to the next solc mode
            // and NOT just bail out of the execution as a whole.
            let build_result = tracing::info_span!("Building contracts").in_scope(|| {
                match leader_state.build_contracts(&mode, self.metadata) {
                    Ok(_) => {
                        tracing::debug!(target = ?Target::Leader, "Contract building succeeded");
                        execution_result.add_successful_build(Target::Leader, mode.clone());
                    },
                    Err(error) => {
                        tracing::error!(target = ?Target::Leader, ?error, "Contract building failed");
                        execution_result.add_failed_build(Target::Leader, mode.clone(), error);
                        return Err(());
                    }
                }
                match follower_state.build_contracts(&mode, self.metadata) {
                    Ok(_) => {
                        tracing::debug!(target = ?Target::Follower, "Contract building succeeded");
                        execution_result.add_successful_build(Target::Follower, mode.clone());
                    },
                    Err(error) => {
                        tracing::error!(target = ?Target::Follower, ?error, "Contract building failed");
                        execution_result.add_failed_build(Target::Follower, mode.clone(), error);
                        return Err(());
                    }
                }
                Ok(())
            });
            if build_result.is_err() {
                // Note: We skip to the next solc mode as there's nothing that we can do at this
                // point, the building has failed. We do NOT bail out of the execution as a whole.
                continue;
            }

            // For cases if one of the inputs fail then we move on to the next case and we do NOT
            // bail out of the whole thing.
            'case_loop: for (case_idx, case) in self.metadata.cases.iter().enumerate() {
                let tracing_span = tracing::info_span!(
                    "Handling case",
                    case_name = case.name,
                    case_idx = case_idx
                );
                let _guard = tracing_span.enter();

                // For inputs if one of the inputs fail we move on to the next case (we do not move
                // on to the next input as it doesn't make sense. It depends on the previous one).
                for (input_idx, input) in case.inputs.iter().enumerate() {
                    let tracing_span = tracing::info_span!("Handling input", input_idx);
                    let _guard = tracing_span.enter();

                    // TODO: verify if this is correct, I doubt that we need to do contract redeploy
                    // for each input. It doesn't quite look to be correct but we need to cross
                    // check with the matterlabs implementation. This matches our implementation but
                    // I have doubts around its correctness.
                    let deployment_result = tracing::info_span!(
                        "Deploying contracts",
                        contract_name = input.instance
                    )
                    .in_scope(|| {
                        if let Err(error) = leader_state.deploy_contracts(input, self.leader_node) {
                            tracing::error!(target = ?Target::Leader, ?error, "Contract deployment failed");
                            execution_result.add_failed_case(
                                Target::Leader,
                                mode.clone(),
                                case.name.clone().unwrap_or("no case name".to_owned()),
                                case_idx,
                                input_idx,
                                anyhow::Error::msg(
                                    format!("Failed to deploy contracts, {error}")
                                )
                            );
                            return Err(error)
                        };
                        if let Err(error) =
                            follower_state.deploy_contracts(input, self.follower_node)
                        {
                            tracing::error!(target = ?Target::Follower, ?error, "Contract deployment failed");
                            execution_result.add_failed_case(
                                Target::Follower,
                                mode.clone(),
                                case.name.clone().unwrap_or("no case name".to_owned()),
                                case_idx,
                                input_idx,
                                anyhow::Error::msg(
                                    format!("Failed to deploy contracts, {error}")
                                )
                            );
                            return Err(error)
                        };
                        Ok(())
                    });
                    if deployment_result.is_err() {
                        // Noting it again here: if something in the input fails we do not move on
                        // to the next input, we move to the next case completely.
                        continue 'case_loop;
                    }

                    let execution_result =
                        tracing::info_span!("Executing input", contract_name = input.instance)
                            .in_scope(|| {
                                let (leader_receipt, _, leader_diff) =
                                    match leader_state.execute_input(input, self.leader_node) {
                                        Ok(result) => result,
                                        Err(error) => {
                                            tracing::error!(
                                                target = ?Target::Leader,
                                                ?error,
                                                "Contract execution failed"
                                            );
                                            return Err(error);
                                        }
                                    };

                                let (follower_receipt, _, follower_diff) =
                                    match follower_state.execute_input(input, self.follower_node) {
                                        Ok(result) => result,
                                        Err(error) => {
                                            tracing::error!(
                                                target = ?Target::Follower,
                                                ?error,
                                                "Contract execution failed"
                                            );
                                            return Err(error);
                                        }
                                    };

                                Ok((leader_receipt, leader_diff, follower_receipt, follower_diff))
                            });
                    let Ok((leader_receipt, leader_diff, follower_receipt, follower_diff)) =
                        execution_result
                    else {
                        // Noting it again here: if something in the input fails we do not move on
                        // to the next input, we move to the next case completely.
                        continue 'case_loop;
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

                // Note: Only consider the case as having been successful after we have processed
                // all of the inputs and completed the entire loop over the input.
                execution_result.add_successful_case(
                    Target::Leader,
                    mode.clone(),
                    case.name.clone().unwrap_or("no case name".to_owned()),
                    case_idx,
                );
                execution_result.add_successful_case(
                    Target::Follower,
                    mode.clone(),
                    case.name.clone().unwrap_or("no case name".to_owned()),
                    case_idx,
                );
            }
        }

        execution_result
    }
}

#[derive(Debug, Default)]
pub struct ExecutionResult {
    pub results: Vec<Box<dyn ExecutionResultItem>>,
    pub successful_cases_count: usize,
    pub failed_cases_count: usize,
}

impl ExecutionResult {
    pub fn new() -> Self {
        Self {
            results: Default::default(),
            successful_cases_count: Default::default(),
            failed_cases_count: Default::default(),
        }
    }

    pub fn add_successful_build(&mut self, target: Target, solc_mode: SolcMode) {
        self.results
            .push(Box::new(BuildResult::Success { target, solc_mode }));
    }

    pub fn add_failed_build(&mut self, target: Target, solc_mode: SolcMode, error: anyhow::Error) {
        self.results.push(Box::new(BuildResult::Failure {
            target,
            solc_mode,
            error,
        }));
    }

    pub fn add_successful_case(
        &mut self,
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: usize,
    ) {
        self.successful_cases_count += 1;
        self.results.push(Box::new(CaseResult::Success {
            target,
            solc_mode,
            case_name,
            case_idx,
        }));
    }

    pub fn add_failed_case(
        &mut self,
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: usize,
        input_idx: usize,
        error: anyhow::Error,
    ) {
        self.failed_cases_count += 1;
        self.results.push(Box::new(CaseResult::Failure {
            target,
            solc_mode,
            case_name,
            case_idx,
            error,
            input_idx,
        }));
    }
}

pub trait ExecutionResultItem: Debug {
    /// Converts this result item into an [`anyhow::Result`].
    fn as_result(&self) -> Result<(), &anyhow::Error>;

    /// Provides information on whether the provided result item is of a success or failure.
    fn is_success(&self) -> bool;

    /// Provides information of the target that this result is for.
    fn target(&self) -> &Target;

    /// Provides information on the [`SolcMode`] mode that we being used for this result item.
    fn solc_mode(&self) -> &SolcMode;

    /// Provides information on the case name and number that this result item pertains to. This is
    /// [`None`] if the error doesn't belong to any case (e.g., if it's a build error outside of any
    /// of the cases.).
    fn case_name_and_index(&self) -> Option<(&str, usize)>;

    /// Provides information on the input number that this result item pertains to. This is [`None`]
    /// if the error doesn't belong to any input (e.g., if it's a build error outside of any of the
    /// inputs.).
    fn input_index(&self) -> Option<usize>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Leader,
    Follower,
}

#[derive(Debug)]
pub enum BuildResult {
    Success {
        target: Target,
        solc_mode: SolcMode,
    },
    Failure {
        target: Target,
        solc_mode: SolcMode,
        error: anyhow::Error,
    },
}

impl ExecutionResultItem for BuildResult {
    fn as_result(&self) -> Result<(), &anyhow::Error> {
        match self {
            Self::Success { .. } => Ok(()),
            Self::Failure { error, .. } => Err(error)?,
        }
    }

    fn is_success(&self) -> bool {
        match self {
            Self::Success { .. } => true,
            Self::Failure { .. } => false,
        }
    }

    fn target(&self) -> &Target {
        match self {
            Self::Success { target, .. } | Self::Failure { target, .. } => target,
        }
    }

    fn solc_mode(&self) -> &SolcMode {
        match self {
            Self::Success { solc_mode, .. } | Self::Failure { solc_mode, .. } => solc_mode,
        }
    }

    fn case_name_and_index(&self) -> Option<(&str, usize)> {
        None
    }

    fn input_index(&self) -> Option<usize> {
        None
    }
}

#[derive(Debug)]
pub enum CaseResult {
    Success {
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: usize,
    },
    Failure {
        target: Target,
        solc_mode: SolcMode,
        case_name: String,
        case_idx: usize,
        input_idx: usize,
        error: anyhow::Error,
    },
}

impl ExecutionResultItem for CaseResult {
    fn as_result(&self) -> Result<(), &anyhow::Error> {
        match self {
            Self::Success { .. } => Ok(()),
            Self::Failure { error, .. } => Err(error)?,
        }
    }

    fn is_success(&self) -> bool {
        match self {
            Self::Success { .. } => true,
            Self::Failure { .. } => false,
        }
    }

    fn target(&self) -> &Target {
        match self {
            Self::Success { target, .. } | Self::Failure { target, .. } => target,
        }
    }

    fn solc_mode(&self) -> &SolcMode {
        match self {
            Self::Success { solc_mode, .. } | Self::Failure { solc_mode, .. } => solc_mode,
        }
    }

    fn case_name_and_index(&self) -> Option<(&str, usize)> {
        match self {
            Self::Success {
                case_name,
                case_idx,
                ..
            }
            | Self::Failure {
                case_name,
                case_idx,
                ..
            } => Some((case_name, *case_idx)),
        }
    }

    fn input_index(&self) -> Option<usize> {
        match self {
            CaseResult::Success { .. } => None,
            CaseResult::Failure { input_idx, .. } => Some(*input_idx),
        }
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
