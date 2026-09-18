use crate::internal_prelude::*;

type RuntimeHeader = GenericHeader<u32, BlakeTwo256>;

pub(crate) struct Profiling<'a> {
    pub runtime: &'a ProfilingRuntime,
    pub externalities: TestExternalities,
    pub metadata: RuntimeMetadata,
    pub signers: HashMap<Address, &'a PrivateKeySigner>,
    pub allocator: PrivateKeyAllocator,
    pub contracts: BTreeMap<ContractInstance, DeployedContract>,
    pub deployments: BTreeMap<Address, ContractInstance>,
    pub variables: HashMap<String, U256>,
    pub parent: RuntimeHeader,
    pub timestamp: u64,
    pub chain_id: u64,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub event_capacity: usize,
    pub report: &'a mut ProfilingReport,
}

impl<'a> Profiling<'a> {
    pub fn new(
        runtime: &'a ProfilingRuntime,
        signers: &'a [PrivateKeySigner],
        wallet: &WalletConfiguration,
        report: &'a mut ProfilingReport,
    ) -> Result<Self> {
        let signers = signers
            .iter()
            .map(|signer| (signer.address(), signer))
            .collect::<HashMap<_, _>>();
        let mut externalities = TestExternalities::default();
        let default = runtime.call(
            &mut externalities.ext(),
            "GenesisBuilder_get_preset",
            &Option::<String>::None.encode(),
            0,
        )?;
        let default = Option::<Vec<u8>>::decode(&mut default.output.as_slice())?
            .context("Missing default genesis")?;
        let development = runtime.call(
            &mut externalities.ext(),
            "GenesisBuilder_get_preset",
            &Some("development").encode(),
            0,
        )?;
        let development = Option::<Vec<u8>>::decode(&mut development.output.as_slice())?
            .context("Missing development genesis preset")?;
        let mut genesis = serde_json::from_slice::<Value>(&default)?;
        merge_genesis(&mut genesis, serde_json::from_slice(&development)?);
        let balances = genesis["balances"]["balances"]
            .as_array_mut()
            .context("Genesis has no balances")?;
        for signer in signers.values() {
            let mut account = [0xee; 32];
            account[..20].copy_from_slice(signer.address().as_slice());
            balances.push(json!([
                AccountId32::new(account).to_ss58check(),
                10_000_000_000_000_000_000_000_000_u128
            ]));
        }
        let built = runtime.call(
            &mut externalities.ext(),
            "GenesisBuilder_build_state",
            &serde_json::to_vec(&genesis)?.encode(),
            0,
        )?;
        std::result::Result::<(), String>::decode(&mut built.output.as_slice())?
            .map_err(Error::msg)?;
        let metadata = runtime.call(&mut externalities.ext(), "Metadata_metadata", &[], 0)?;
        let encoded = Vec::<u8>::decode(&mut metadata.output.as_slice())?;
        let metadata = RuntimeMetadata::decode(&mut encoded.as_slice())?;
        let chain_id = metadata
            .pallet_by_name("Revive")
            .and_then(|pallet| pallet.constant_by_name("ChainId"))
            .context("Runtime has no Revive ChainId")?;
        let chain_id = u64::decode(&mut chain_id.value())?;
        let parent = RuntimeHeader::new(
            0,
            Default::default(),
            Default::default(),
            Default::default(),
            Digest::default(),
        );
        let gas_limit = runtime
            .call(
                &mut externalities.ext(),
                "ReviveApi_max_extrinsic_weight_in_gas",
                &[],
                0,
            )?
            .output;
        let gas_limit =
            u64::try_from(RuntimeU256::decode(&mut gas_limit.as_slice())?).map_err(Error::msg)?;
        let gas_price = runtime
            .call(&mut externalities.ext(), "ReviveApi_gas_price", &[], 0)?
            .output;
        let gas_price =
            u128::try_from(RuntimeU256::decode(&mut gas_price.as_slice())?).map_err(Error::msg)?;
        Ok(Self {
            runtime,
            externalities,
            metadata,
            signers,
            allocator: PrivateKeyAllocator::new(U256::from(wallet.additional_keys)),
            contracts: BTreeMap::new(),
            deployments: BTreeMap::new(),
            variables: HashMap::new(),
            parent,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            chain_id,
            gas_limit,
            gas_price,
            event_capacity: 2_097_152,
            report,
        })
    }

    pub fn register_contract(
        &mut self,
        instance: ContractInstance,
        address: Address,
        abi: JsonAbi,
    ) {
        self.deployments.insert(address, instance.clone());
        self.contracts
            .insert(instance, DeployedContract { address, abi });
    }

    pub fn deploy_library(&mut self, caller: Address, bytecode: Vec<u8>) -> Result<Address> {
        let nonce = self.api::<u32>(
            "ReviveApi_nonce",
            &RuntimeAddress::from_slice(caller.as_slice()).encode(),
        )?;
        let result = self.transact(
            TransactionRequest::default()
                .with_from(caller)
                .with_deploy_code(bytecode),
        )?;
        ensure!(
            matches!(result.output, TransactionOutput::Returned(_)),
            "Library deployment failed"
        );
        self.report.write_transaction(&result.report)?;
        Ok(caller.create(u64::from(nonce)))
    }

    pub async fn run(
        &mut self,
        metadata: &MetadataFile,
        case: &Case,
        compiled: &CompilerOutput,
        version: &Version,
    ) -> Result<()> {
        for (index, step) in case.steps_iterator().enumerate() {
            self.step(
                &step,
                metadata,
                compiled,
                version,
                &StepPath::new(vec![index.into()]),
                None,
            )
            .await?;
        }
        Ok(())
    }

    async fn step(
        &mut self,
        step: &Step,
        metadata: &MetadataFile,
        compiled: &CompilerOutput,
        version: &Version,
        path: &StepPath,
        repeat_path: Option<&StepPath>,
    ) -> Result<()> {
        let transaction = match step {
            Step::Repeat(repeat) => {
                for index in 0..repeat.repeat {
                    if let Some(name) = &repeat.capture_index {
                        self.variables.insert(
                            name.trim_start_matches("$VARIABLE:").to_owned(),
                            U256::from(index),
                        );
                    }
                    for (step_index, step) in repeat.steps.iter().enumerate() {
                        Box::pin(self.step(
                            step,
                            metadata,
                            compiled,
                            version,
                            &path.append(step_index),
                            Some(path),
                        ))
                        .await?;
                    }
                }
                None
            }
            Step::AllocateAccount(step) => {
                let address = self.allocator.allocate()?.address();
                let name = self
                    .variable_name(step.variable_name.trim_start_matches("$VARIABLE:"))
                    .await?;
                self.variables
                    .insert(name, U256::from_be_slice(address.as_slice()));
                None
            }
            Step::Transfer(step) => {
                let request = TransactionRequest::default()
                    .with_from(self.address(&step.from).await?)
                    .with_to(self.address(&step.to).await?)
                    .with_value(step.amount.into_inner());
                let result = self.transact(request)?;
                ensure!(
                    matches!(result.output, TransactionOutput::Returned(_)),
                    "Transfer failed"
                );
                Some(result.report)
            }
            Step::BalanceAssertion(step) => {
                let address = self.address(&step.address).await?;
                let actual = self.api::<RuntimeU256>(
                    "ReviveApi_balance",
                    &RuntimeAddress::from_slice(address.as_slice()).encode(),
                )?;
                ensure!(
                    actual
                        == RuntimeU256::from_big_endian(&step.expected_balance.to_be_bytes::<32>()),
                    "Balance assertion failed at {address}: {actual}"
                );
                None
            }
            Step::StorageEmptyAssertion(step) => {
                let address = self.address(&step.address).await?;
                let key = subxt_core::storage::get_address_bytes(
                    &subxt::dynamic::storage(
                        "Revive",
                        "ContractInfoOf",
                        vec![DynamicValue::from_bytes(address)],
                    ),
                    &self.metadata,
                )?;
                let info = self
                    .externalities
                    .execute_with(|| sp_io::storage::get(&key));
                let empty = match info {
                    None => true,
                    Some(bytes) => {
                        let trie = Vec::<u8>::decode(&mut bytes.as_ref())?;
                        let child = ChildInfo::new_default(&trie);
                        self.externalities
                            .ext()
                            .next_child_storage_key(&child, &[])
                            .is_none()
                    }
                };
                ensure!(
                    empty == step.is_storage_empty,
                    "Storage assertion failed at {address}"
                );
                None
            }
            Step::FunctionCall(step) => Some(
                self.function_call(step, metadata, compiled, version)
                    .await?,
            ),
        };
        if let Some(mut transaction) = transaction {
            transaction.repeat_path = repeat_path.map(|_| path.clone());
            self.report.write_transaction(&transaction)?;
        }
        Ok(())
    }

    async fn function_call(
        &mut self,
        step: &FunctionCallStep,
        metadata: &MetadataFile,
        compiled: &CompilerOutput,
        version: &Version,
    ) -> Result<TransactionProfilingReport> {
        ensure!(
            step.storage.as_ref().is_none_or(HashMap::is_empty),
            "Profiling does not accept storage overrides"
        );
        let instance = match &step.instance {
            ContractInstanceOrReference::Instance(instance) => instance.as_ref().clone(),
            ContractInstanceOrReference::Reference(reference) => {
                let reference = reference.to_string();
                let index = self
                    .expression(reference.trim_start_matches("$INSTANCE:"))
                    .await?;
                metadata
                    .contracts
                    .as_ref()
                    .and_then(|contracts| contracts.get_index(usize::try_from(index).ok()?))
                    .map(|(instance, _)| instance.clone())
                    .context("Invalid contract instance reference")?
            }
        };
        let caller = self.address(&step.caller).await?;
        let arguments = self.calldata(&step.calldata).await?;
        let mut request = TransactionRequest::default().with_from(caller).with_value(
            step.value
                .map(|value| value.into_inner())
                .unwrap_or_default(),
        );
        let deployment = match &step.method {
            Method::Deployer => {
                let sources = metadata.contract_sources(CompilerIdentifier::Solc)?;
                let source = sources
                    .get(&instance)
                    .context("Unknown contract instance")?;
                let (bytecode, abi) = compiled
                    .contracts
                    .get(&source.contract_source_path)
                    .and_then(|contracts| contracts.get(source.contract_ident.as_str()))
                    .context("Compiled contract missing")?;
                let mut input = hex::decode(bytecode)?;
                input.extend(arguments);
                request = request.with_deploy_code(input);
                let nonce = self.api::<u32>(
                    "ReviveApi_nonce",
                    &RuntimeAddress::from_slice(caller.as_slice()).encode(),
                )?;
                Some(DeployedContract {
                    address: caller.create(u64::from(nonce)),
                    abi: abi.clone(),
                })
            }
            Method::Fallback | Method::Function(_) => {
                let contract = self
                    .contracts
                    .get(&instance)
                    .context("Contract has not been deployed")?;
                let input = match &step.method {
                    Method::Function(name) => {
                        let selector =
                            match name {
                                NameOrSelector::Selector { selector, .. } => *selector,
                                NameOrSelector::FunctionName(name) => contract
                                    .abi
                                    .functions
                                    .get(name)
                                    .filter(|functions| functions.len() == 1)
                                    .and_then(|functions| functions.first())
                                    .context(
                                        "Function name is absent or ambiguous; use its signature",
                                    )?
                                    .selector()
                                    .0,
                            };
                        [selector.as_slice(), arguments.as_slice()].concat()
                    }
                    Method::Fallback => arguments,
                    Method::Deployer => unreachable!(),
                };
                request = request.with_to(contract.address).with_input(input);
                None
            }
        };
        if let Some(gas) = step.gas_overrides.get(&PlatformName::new("REVM")) {
            gas.apply_to::<Ethereum>(&mut request);
        }
        let mut result = self.transact(request)?;
        let method = match &step.method {
            Method::Deployer => "constructor",
            Method::Fallback => "fallback",
            Method::Function(NameOrSelector::FunctionName(name))
            | Method::Function(NameOrSelector::Selector {
                function_name: name,
                ..
            }) => name,
        };
        result.report.entry_point = Some(format!("{instance}.{method}"));
        let expected = match &step.expected {
            Some(Expected::Expected(expected)) => vec![expected],
            Some(Expected::ExpectedMany(expected)) => expected.iter().collect(),
            Some(Expected::Calldata(_)) | None => Vec::new(),
        };
        let expected = expected
            .into_iter()
            .filter(|expected| {
                expected
                    .compiler_version
                    .as_ref()
                    .is_none_or(|requirement| requirement.matches(version))
            })
            .collect::<Vec<_>>();
        let failed = !matches!(result.output, TransactionOutput::Returned(_));
        ensure!(
            failed == expected.iter().any(|expected| expected.exception),
            "Unexpected transaction outcome: {:?}",
            result.output
        );
        let return_data = match (&result.output, &deployment) {
            (TransactionOutput::Returned(_), Some(contract)) => {
                Cow::Owned(contract.address.into_word().to_vec())
            }
            (TransactionOutput::Returned(data) | TransactionOutput::Reverted(data), _) => {
                Cow::Borrowed(data.as_slice())
            }
            (TransactionOutput::Failed, _) => Cow::Borrowed([].as_slice()),
        };
        if let Some(deployment) = deployment
            && !failed
        {
            self.register_contract(instance, deployment.address, deployment.abi);
        }
        if let Some(Expected::Calldata(expected)) = &step.expected {
            self.assert_calldata(expected, &return_data).await?;
        }
        for expected in expected {
            if let Some(data) = &expected.return_data {
                self.assert_calldata(data, &return_data).await?;
            }
            if let Some(events) = &expected.events {
                ensure!(
                    events.len() == result.logs.len(),
                    "Unexpected number of contract events"
                );
                for (expected, actual) in events.iter().zip(&result.logs) {
                    if let Some(address) = &expected.address {
                        ensure!(
                            self.address(address).await? == actual.address,
                            "Unexpected event address"
                        );
                    }
                    ensure!(
                        expected.topics.len() == actual.topics.len(),
                        "Unexpected event topic count"
                    );
                    for (expected, actual) in expected.topics.iter().zip(&actual.topics) {
                        self.assert_calldata(
                            &Calldata::new_compound([expected]),
                            actual.as_slice(),
                        )
                        .await?;
                    }
                    self.assert_calldata(&expected.values, &actual.data).await?;
                }
            }
        }
        if let Some(assignments) = &step.variable_assignments {
            let values = match assignments {
                VariableAssignments::ReturnData { names } => {
                    ensure!(
                        return_data.len() == names.len() * 32,
                        "Return-data assignment count does not match output"
                    );
                    return_data
                        .chunks_exact(32)
                        .map(U256::from_be_slice)
                        .collect::<Vec<_>>()
                }
                VariableAssignments::EventTopics { topics, .. } => topics
                    .iter()
                    .map(|topic| {
                        result
                            .logs
                            .get(topic.log_index)
                            .and_then(|log| log.topics.get(topic.topic_index))
                            .map(|topic| U256::from_be_slice(topic.as_slice()))
                            .context("Assigned event topic is missing")
                    })
                    .collect::<Result<Vec<_>>>()?,
            };
            ensure!(
                assignments.names().len() == values.len(),
                "Variable assignment count mismatch"
            );
            for (name, value) in assignments.names().iter().zip(values) {
                let name = self
                    .variable_name(name.trim_start_matches("$VARIABLE:"))
                    .await?;
                self.variables.insert(name, value);
            }
        }
        Ok(result.report)
    }

    fn transact(&mut self, mut request: TransactionRequest) -> Result<TransactionResult> {
        self.initialize_block()?;
        let caller = request.from.context("Transaction has no sender")?;
        let destination = request.to.and_then(|kind| kind.to().copied());
        let nonce = self.api::<u32>(
            "ReviveApi_nonce",
            &RuntimeAddress::from_slice(caller.as_slice()).encode(),
        )?;
        request.set_nonce(u64::from(nonce));
        request.set_chain_id(self.chain_id);
        if request.gas.is_none() {
            request.set_gas_limit(self.gas_limit);
        }
        if request.gas_price.is_none() && request.max_fee_per_gas.is_none() {
            request.set_gas_price(self.gas_price);
        }
        let mut transaction = request
            .build_unsigned()
            .map_err(|error| anyhow!("Invalid transaction: {error:?}"))?;
        let signer = self
            .signers
            .get(&caller)
            .with_context(|| format!("Wallet has no signer for {caller}"))?;
        let signature = signer.sign_transaction_sync(&mut transaction)?;
        let envelope = TxEnvelope::from(transaction.into_signed(signature));
        let transaction_hash = *envelope.tx_hash();
        let payload = envelope.encoded_2718();
        let call = self.encode_call("Revive", "eth_transact", &payload.encode())?;
        let extrinsic =
            UncheckedExtrinsic::<AccountId32, Encoded, MultiSignature, ()>::new_bare(Encoded(call));
        let started = Instant::now();
        let execution = self.apply(extrinsic, self.event_capacity)?;
        self.event_capacity = self
            .event_capacity
            .max(execution.events.raw_events.capacity());
        let events_key = [
            sp_io::hashing::twox_128(b"System"),
            sp_io::hashing::twox_128(b"Events"),
        ]
        .concat();
        let bytes = self
            .externalities
            .execute_with(|| sp_io::storage::get(&events_key))
            .context("Missing runtime events")?;
        let mut logs = Vec::new();
        let mut output = execution.transaction_output;
        for event in
            RuntimeEvents::<PolkadotConfig>::decode_from(bytes.to_vec(), self.metadata.clone())
                .iter()
        {
            let event = event?;
            match (event.pallet_name(), event.variant_name()) {
                ("Revive", "ContractEmitted") => {
                    let (address, data, topics) =
                        <(RuntimeAddress, Vec<u8>, Vec<H256>)>::decode(&mut event.field_bytes())?;
                    logs.push(ContractLog {
                        address: Address::from_slice(address.as_bytes()),
                        data,
                        topics: topics
                            .into_iter()
                            .map(|topic| B256::from(topic.0))
                            .collect(),
                    });
                }
                ("Revive", "EthExtrinsicRevert")
                    if !matches!(output, Some(TransactionOutput::Reverted(_))) =>
                {
                    output = Some(TransactionOutput::Failed);
                }
                _ => {}
            }
        }
        let output = output.context("Transaction completed without a captured return value")?;
        self.parent = self.api::<RuntimeHeader>("BlockBuilder_finalize_block", &[])?;
        let report = TransactionProfilingReport {
            transaction_hash,
            sender: caller,
            destination,
            nonce: u64::from(nonce),
            repeat_path: None,
            entry_point: None,
            events: execution.events.relative_to(started)?,
        };
        Ok(TransactionResult {
            output,
            logs,
            report,
        })
    }

    fn initialize_block(&mut self) -> Result<()> {
        let slot_duration = self.api::<u64>("AuraApi_slot_duration", &[])?;
        self.timestamp = (self.timestamp / slot_duration + 1) * slot_duration;
        let number = self.parent.number() + 1;
        let header = RuntimeHeader::new(
            number,
            Default::default(),
            Default::default(),
            self.parent.hash(),
            Digest {
                logs: vec![DigestItem::PreRuntime(
                    *b"aura",
                    (self.timestamp / slot_duration).encode(),
                )],
            },
        );
        self.runtime.call(
            &mut self.externalities.ext(),
            "Core_initialize_block",
            &header.encode(),
            0,
        )?;
        let offset = self.api::<u32>("RelayParentOffsetApi_relay_parent_offset", &[])?;
        let (relay_parent_storage_root, relay_chain_state) = RelayStateSproofBuilder {
            para_id: 1000.into(),
            current_slot: (self.timestamp / 6000).into(),
            included_para_head: Some(relay_chain::HeadData(self.parent.encode())),
            ..Default::default()
        }
        .into_state_root_and_proof();
        let validation = ParachainInherentData {
            validation_data: PersistedValidationData {
                parent_head: relay_chain::HeadData(self.parent.encode()),
                relay_parent_number: number,
                relay_parent_storage_root,
                max_pov_size: relay_chain::MAX_POV_SIZE,
            },
            relay_chain_state,
            downward_messages: Default::default(),
            horizontal_messages: Default::default(),
            relay_parent_descendants: build_relay_parent_descendants(
                u64::from(offset) + 1,
                relay_parent_storage_root,
                generate_authority_pairs(1),
            ),
            collator_peer_id: None,
        };
        let mut inherents = InherentData::new();
        inherents.put_data(PARACHAIN_INHERENT_IDENTIFIER, &validation)?;
        inherents.put_data(TIMESTAMP_INHERENT_IDENTIFIER, &self.timestamp)?;
        for extrinsic in self
            .api::<Vec<OpaqueExtrinsic>>("BlockBuilder_inherent_extrinsics", &inherents.encode())?
        {
            self.apply(extrinsic, 0)?;
        }
        Ok(())
    }

    fn encode_call(&self, pallet: &str, method: &str, arguments: &[u8]) -> Result<Vec<u8>> {
        let pallet = self
            .metadata
            .pallet_by_name(pallet)
            .context("Pallet missing from runtime")?;
        let call = pallet
            .call_variant_by_name(method)
            .context("Call missing from runtime")?;
        Ok([&[pallet.index(), call.index], arguments].concat())
    }

    fn apply(&mut self, extrinsic: impl Encode, capacity: usize) -> Result<ProfiledCall> {
        let result = self.runtime.call(
            &mut self.externalities.ext(),
            "BlockBuilder_apply_extrinsic",
            &extrinsic.encode(),
            capacity,
        )?;
        ApplyExtrinsicResult::decode(&mut result.output.as_slice())?
            .map_err(|error| anyhow!("Transaction is invalid: {error:?}"))?
            .map_err(|error| anyhow!("Transaction dispatch failed: {error:?}"))?;
        Ok(result)
    }

    fn api<T: Decode>(&mut self, method: &str, input: &[u8]) -> Result<T> {
        let result = self
            .runtime
            .call(&mut self.externalities.ext(), method, input, 0)?;
        T::decode(&mut result.output.as_slice())
            .with_context(|| format!("Invalid output from {method}"))
    }

    async fn address(&mut self, address: &StepAddress) -> Result<Address> {
        match address {
            StepAddress::Address(address) => Ok(*address),
            StepAddress::ResolvableAddress(expression) => Ok(Address::from_word(B256::from(
                self.expression(expression).await?.to_be_bytes::<32>(),
            ))),
        }
    }

    async fn calldata(&mut self, calldata: &Calldata) -> Result<Vec<u8>> {
        match calldata {
            Calldata::Single(bytes) => Ok(bytes.to_vec()),
            Calldata::Compound(items) => {
                let mut bytes = Vec::new();
                for item in items {
                    bytes.extend(self.expression(item.as_ref()).await?.to_be_bytes::<32>());
                }
                Ok(bytes)
            }
        }
    }

    async fn assert_calldata(&mut self, expected: &Calldata, actual: &[u8]) -> Result<()> {
        match expected {
            Calldata::Single(bytes) => {
                ensure!(bytes.as_ref() == actual, "Return data does not match")
            }
            Calldata::Compound(items) => {
                ensure!(
                    items.len() == actual.len().div_ceil(32),
                    "Return-data length does not match"
                );
                for (item, actual) in items.iter().zip(actual.chunks(32)) {
                    if item.as_ref() != "*" {
                        let mut padded = [0; 32];
                        padded[..actual.len()].copy_from_slice(actual);
                        ensure!(
                            self.expression(item.as_ref()).await? == U256::from_be_bytes(padded),
                            "Return data does not match"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    async fn expression(&mut self, expression: &str) -> Result<U256> {
        let normalized = expression.split_whitespace().collect::<Vec<_>>().join(" ");
        CalldataItem::new(if normalized.is_empty() {
            "0"
        } else {
            &normalized
        })
        .resolve(&mut self.resolution_context())
        .await
    }

    async fn variable_name(&mut self, name: &str) -> Result<String> {
        CalldataToken::<&str>::resolve_variable_name_template(name, &mut self.resolution_context())
            .await
    }

    fn resolution_context(&mut self) -> ResolutionContext<'_, Self> {
        ResolutionContext {
            metadata: None,
            pinned_block: None,
            transaction_hash: None,
            node_connector: None,
            api: Some(self),
        }
    }
}

impl LazyResolverApi for Profiling<'_> {
    fn runtime_value(&mut self, token: &str) -> Option<Result<U256>> {
        Some(match token {
            "$CHAIN_ID" => Ok(U256::from(self.chain_id)),
            "$GAS_LIMIT" => self
                .api::<RuntimeU256>("ReviveApi_block_gas_limit", &[])
                .map(|limit| U256::from(limit.as_u64())),
            "$BASE_FEE" | "$TRANSACTION_GAS_PRICE" => Ok(U256::from(self.gas_price)),
            "$BLOCK_NUMBER" => Ok(U256::from(*self.parent.number())),
            "$BLOCK_TIMESTAMP" => Ok(U256::from(self.timestamp / 1000)),
            _ => return None,
        })
    }

    async fn get_contract_address(
        &mut self,
        contract_ref: &ContractInstanceOrReference<'_>,
    ) -> Result<Address> {
        let ContractInstanceOrReference::Instance(instance) = contract_ref else {
            bail!("Instance references are not supported in profiling calldata")
        };
        let instance = ContractInstance::new(self.variable_name(instance.as_inner()).await?);
        self.contracts
            .get(&instance)
            .map(|contract| contract.address)
            .context("Unknown contract address")
    }

    async fn get_variable(&mut self, variable: impl AsRef<str>) -> Option<Result<U256>> {
        self.variables.get(variable.as_ref()).copied().map(Ok)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DeployedContract {
    pub address: Address,
    pub abi: JsonAbi,
}

struct TransactionResult {
    output: TransactionOutput,
    logs: Vec<ContractLog>,
    report: TransactionProfilingReport,
}

struct ContractLog {
    address: Address,
    data: Vec<u8>,
    topics: Vec<B256>,
}

fn merge_genesis(genesis: &mut Value, patch: Value) {
    match (genesis, patch) {
        (Value::Object(genesis), Value::Object(patch)) => {
            for (name, value) in patch {
                merge_genesis(genesis.entry(name).or_insert(Value::Null), value);
            }
        }
        (genesis, patch) => *genesis = patch,
    }
}

#[derive(Debug)]
pub(crate) struct ProfilingReport {
    pub writer: BufWriter<File>,
    pub path: PathBuf,
    pub temporary_path: PathBuf,
    pub workload_count: usize,
    pub transaction_count: usize,
}

impl ProfilingReport {
    pub fn new(
        working_directory: impl AsRef<Path>,
        runtime_branch: impl AsRef<str>,
        runtime_commit: impl AsRef<str>,
    ) -> Result<Self> {
        let directory = working_directory.as_ref();
        create_dir_all(directory).with_context(|| {
            format!("Failed to create report directory {}", directory.display())
        })?;
        let path = directory.join("profiling_report.json");
        let temporary_path = directory.join("profiling_report.json.partial");
        let writer = BufWriter::new(File::create(&temporary_path).with_context(|| {
            format!(
                "Failed to create profiling report {}",
                temporary_path.display()
            )
        })?);
        let mut report = Self {
            writer,
            path,
            temporary_path,
            workload_count: 0,
            transaction_count: 0,
        };
        report.writer.write_all(b"{\"runtime_branch\":")?;
        serde_json::to_writer(&mut report.writer, runtime_branch.as_ref())?;
        report.write_field("runtime_commit", &runtime_commit.as_ref())?;
        report.write_field("machine", &current_machine_information::get())?;
        report.writer.write_all(b",\"workloads\":[")?;
        Ok(report)
    }

    pub fn begin_workload(&mut self) -> Result<()> {
        if self.workload_count > 0 {
            self.writer.write_all(b",")?;
        }
        self.writer.write_all(b"{\"transactions\":[")?;
        self.transaction_count = 0;
        Ok(())
    }

    pub fn write_transaction(&mut self, transaction: &TransactionProfilingReport) -> Result<()> {
        if self.transaction_count > 0 {
            self.writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut self.writer, transaction)
            .context("Failed to write profiling transaction")?;
        self.transaction_count += 1;
        Ok(())
    }

    pub fn finish_workload(&mut self, workload: &WorkloadProfilingReport) -> Result<()> {
        self.writer.write_all(b"]")?;
        self.write_field("metadata_file_path", &workload.metadata_file_path)?;
        self.write_field("case_index", &workload.case_index)?;
        self.write_field("mode", &workload.mode)?;
        self.write_field("name", &workload.name)?;
        self.write_field("function_names", &workload.function_names)?;
        self.write_field("deployments", &workload.deployments)?;
        self.writer.write_all(b"}")?;
        self.workload_count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        ensure!(
            self.workload_count > 0,
            "No compatible EVM workloads matched the selection"
        );
        self.writer.write_all(b"]}")?;
        self.writer.flush().with_context(|| {
            format!(
                "Failed to flush profiling report {}",
                self.temporary_path.display()
            )
        })?;
        let Self {
            writer,
            path,
            temporary_path,
            ..
        } = self;
        drop(writer);
        rename(&temporary_path, &path)
            .with_context(|| format!("Failed to publish profiling report {}", path.display()))?;
        Ok(path)
    }

    fn write_field(&mut self, name: &str, value: &impl Serialize) -> Result<()> {
        self.writer.write_all(b",")?;
        serde_json::to_writer(&mut self.writer, name)?;
        self.writer.write_all(b":")?;
        serde_json::to_writer(&mut self.writer, value)?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkloadProfilingReport {
    pub metadata_file_path: PathBuf,
    pub case_index: CaseIdx,
    pub mode: Mode,
    pub name: Option<String>,
    pub function_names: BTreeMap<String, BTreeSet<String>>,
    pub deployments: BTreeMap<Address, String>,
}

// Event offsets and measurement starts are relative to the signed runtime call.
#[derive(Debug, Serialize)]
pub(crate) struct TransactionProfilingReport {
    pub transaction_hash: TxHash,
    pub sender: Address,
    pub destination: Option<Address>,
    pub nonce: u64,
    pub repeat_path: Option<StepPath>,
    pub entry_point: Option<String>,
    #[serde(flatten)]
    pub events: ProcessedEventsAndRawEvents<Duration>,
}
