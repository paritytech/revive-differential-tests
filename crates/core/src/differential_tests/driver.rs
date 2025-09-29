use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use alloy::{
    network::{Ethereum, TransactionBuilder},
    rpc::types::TransactionRequest,
};
use anyhow::{Context as _, Result};
use futures::{StreamExt, stream};
use revive_dt_common::types::{PlatformIdentifier, PrivateKeyAllocator};
use revive_dt_format::{
    metadata::ContractPathAndIdent,
    steps::{FunctionCallStep, Step},
};
use tracing::{debug, error};

use crate::{
    differential_tests::ExecutionState,
    helpers::{CachedCompiler, TestDefinition},
};

pub struct DifferentialTestsDriver<'a> {
    /// The definition of the test that the driver is instructed to execute.
    test_definition: TestDefinition<'a>,

    /// The private key allocator used by this driver and other drivers when account allocations are
    /// needed.
    private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,

    /// The execution state associated with each one of the platforms that the test definition
    /// is instructed to run on.
    execution_state: BTreeMap<PlatformIdentifier, ExecutionState>,
    // TODO(driver-refactor): Explore the idea of keeping an iterator over the steps in here that
    // provides both the steps and the step path.
}

impl<'a> DifferentialTestsDriver<'a> {
    pub async fn new(
        test_definition: TestDefinition<'a>,
        private_key_allocator: Arc<Mutex<PrivateKeyAllocator>>,
        cached_compiler: &CachedCompiler<'a>,
    ) -> Result<Self> {
        let mut this = DifferentialTestsDriver {
            test_definition,
            private_key_allocator,
            execution_state: Default::default(),
        };
        this.init_state(cached_compiler)
            .await
            .context("Failed to initialize the state of the differential tests driver")?;
        Ok(this)
    }

    async fn init_state(&mut self, cached_compiler: &CachedCompiler<'a>) -> Result<()> {
        let test_definition = &self.test_definition;
        self.execution_state = stream::iter(self.test_definition.platforms.iter())
            // Compiling the pre-link contracts.
            .filter_map(|(platform_identifier, platform_information)| async move {
                let compiler_output = cached_compiler
                    .compile_contracts(
                        test_definition.metadata,
                        test_definition.metadata_file_path,
                        test_definition.mode.clone(),
                        None,
                        platform_information.compiler.as_ref(),
                        platform_information.platform,
                        &platform_information.reporter,
                    )
                    .await
                    .inspect_err(|err| {
                        error!(
                            ?err,
                            %platform_identifier,
                            "Pre-linking compilation failed"
                        )
                    })
                    .ok()?;
                Some((platform_identifier, platform_information, compiler_output))
            })
            // Deploying the libraries for the platform.
            .filter_map(
                |(platform_identifier, platform_information, compiler_output)| async move {
                    let mut deployed_libraries = None::<HashMap<_, _>>;
                    let mut contract_sources = test_definition
                        .metadata
                        .contract_sources()
                        .inspect_err(|err| {
                            error!(
                                ?err,
                                %platform_identifier,
                                "Failed to retrieve contract sources from metadata"
                            )
                        })
                        .ok()?;
                    for library_instance in test_definition
                        .metadata
                        .libraries
                        .iter()
                        .flatten()
                        .flat_map(|(_, map)| map.values())
                    {
                        debug!(%library_instance, "Deploying Library Instance");

                        let ContractPathAndIdent {
                            contract_source_path: library_source_path,
                            contract_ident: library_ident,
                        } = contract_sources.remove(library_instance)?;

                        let (code, abi) = compiler_output
                            .contracts
                            .get(&library_source_path)
                            .and_then(|contracts| contracts.get(library_ident.as_str()))?;

                        let code = alloy::hex::decode(code).ok()?;

                        // Getting the deployer address from the cases themselves. This is to ensure
                        // that we're doing the deployments from different accounts and therefore we're
                        // not slowed down by the nonce.
                        let deployer_address = test_definition
                            .case
                            .steps
                            .iter()
                            .filter_map(|step| match step {
                                Step::FunctionCall(input) => input.caller.as_address().copied(),
                                Step::BalanceAssertion(..) => None,
                                Step::StorageEmptyAssertion(..) => None,
                                Step::Repeat(..) => None,
                                Step::AllocateAccount(..) => None,
                            })
                            .next()
                            .unwrap_or(FunctionCallStep::default_caller_address());
                        let tx = TransactionBuilder::<Ethereum>::with_deploy_code(
                            TransactionRequest::default().from(deployer_address),
                            code,
                        );
                        let receipt = platform_information
                            .node
                            .execute_transaction(tx)
                            .await
                            .inspect_err(|err| {
                                error!(
                                    ?err,
                                    %library_instance,
                                    %platform_identifier,
                                    "Failed to deploy the library"
                                )
                            })
                            .ok()?;

                        debug!(
                            ?library_instance,
                            %platform_identifier,
                            "Deployed library"
                        );

                        let library_address = receipt.contract_address?;

                        deployed_libraries.get_or_insert_default().insert(
                            library_instance.clone(),
                            (library_ident.clone(), library_address, abi.clone()),
                        );
                    }

                    Some((
                        platform_identifier,
                        platform_information,
                        compiler_output,
                        deployed_libraries,
                    ))
                },
            )
            // Compiling the post-link contracts.
            .filter_map(
                |(platform_identifier, platform_information, _, deployed_libraries)| async move {
                    let compiler_output = cached_compiler
                        .compile_contracts(
                            test_definition.metadata,
                            test_definition.metadata_file_path,
                            test_definition.mode.clone(),
                            deployed_libraries.as_ref(),
                            platform_information.compiler.as_ref(),
                            platform_information.platform,
                            &platform_information.reporter,
                        )
                        .await
                        .inspect_err(|err| {
                            error!(
                                ?err,
                                %platform_identifier,
                                "Pre-linking compilation failed"
                            )
                        })
                        .ok()?;

                    let state = ExecutionState::new(
                        compiler_output.contracts,
                        deployed_libraries.unwrap_or_default(),
                    );

                    Some((*platform_identifier, state))
                },
            )
            // Collect
            .collect::<BTreeMap<_, _>>()
            .await;

        todo!()
    }
}
