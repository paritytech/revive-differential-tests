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
    pub transactions: Vec<TransactionProfilingReport>,
}

impl<'a> Profiling<'a> {
    pub fn new(
        runtime: &'a ProfilingRuntime,
        signers: &'a [PrivateKeySigner],
        wallet: &WalletConfiguration,
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
            transactions: Vec::new(),
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
        Ok(caller.create(u64::from(nonce)))
    }

    pub fn run(
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
            )?;
        }
        Ok(())
    }

    fn step(
        &mut self,
        step: &Step,
        metadata: &MetadataFile,
        compiled: &CompilerOutput,
        version: &Version,
        path: &StepPath,
        repeat_path: Option<&StepPath>,
    ) -> Result<()> {
        let first_transaction = self.transactions.len();
        match step {
            Step::Repeat(repeat) => {
                for index in 0..repeat.repeat {
                    if let Some(name) = &repeat.capture_index {
                        self.variables.insert(
                            name.trim_start_matches("$VARIABLE:").to_owned(),
                            U256::from(index),
                        );
                    }
                    for (step_index, step) in repeat.steps.iter().enumerate() {
                        self.step(
                            step,
                            metadata,
                            compiled,
                            version,
                            &path.append(step_index),
                            Some(path),
                        )?;
                    }
                }
            }
            Step::AllocateAccount(step) => {
                let address = self.allocator.allocate()?.address();
                self.variables.insert(
                    step.variable_name
                        .trim_start_matches("$VARIABLE:")
                        .to_owned(),
                    U256::from_be_slice(address.as_slice()),
                );
            }
            Step::Transfer(step) => {
                let request = TransactionRequest::default()
                    .with_from(self.address(&step.from)?)
                    .with_to(self.address(&step.to)?)
                    .with_value(step.amount.into_inner());
                let result = self.transact(request)?;
                ensure!(
                    matches!(result.output, TransactionOutput::Returned(_)),
                    "Transfer failed"
                );
            }
            Step::BalanceAssertion(step) => {
                let address = self.address(&step.address)?;
                let actual = self.api::<RuntimeU256>(
                    "ReviveApi_balance",
                    &RuntimeAddress::from_slice(address.as_slice()).encode(),
                )?;
                ensure!(
                    actual
                        == RuntimeU256::from_big_endian(&step.expected_balance.to_be_bytes::<32>()),
                    "Balance assertion failed at {address}: {actual}"
                );
            }
            Step::StorageEmptyAssertion(step) => {
                let address = self.address(&step.address)?;
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
            }
            Step::FunctionCall(step) => self.function_call(step, metadata, compiled, version)?,
        }
        if repeat_path.is_some() && !matches!(step, Step::Repeat(_)) {
            for transaction in &mut self.transactions[first_transaction..] {
                transaction.repeat_path = Some(path.clone());
            }
        }
        Ok(())
    }

    fn function_call(
        &mut self,
        step: &FunctionCallStep,
        metadata: &MetadataFile,
        compiled: &CompilerOutput,
        version: &Version,
    ) -> Result<()> {
        ensure!(
            step.storage.as_ref().is_none_or(HashMap::is_empty),
            "Profiling does not accept storage overrides"
        );
        let instance = match &step.instance {
            ContractInstanceOrReference::Instance(instance) => instance.as_ref().clone(),
            ContractInstanceOrReference::Reference(reference) => {
                let reference = reference.to_string();
                let index = self.expression(reference.trim_start_matches("$INSTANCE:"))?;
                metadata
                    .contracts
                    .as_ref()
                    .and_then(|contracts| contracts.get_index(usize::try_from(index).ok()?))
                    .map(|(instance, _)| instance.clone())
                    .context("Invalid contract instance reference")?
            }
        };
        let caller = self.address(&step.caller)?;
        let arguments = self.calldata(&step.calldata)?;
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
        let result = self.transact(request)?;
        let method = match &step.method {
            Method::Deployer => "constructor",
            Method::Fallback => "fallback",
            Method::Function(NameOrSelector::FunctionName(name))
            | Method::Function(NameOrSelector::Selector {
                function_name: name,
                ..
            }) => name,
        };
        self.transactions
            .last_mut()
            .context("Missing transaction report")?
            .entry_point = Some(format!("{instance}.{method}"));
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
        let reverted = matches!(result.output, TransactionOutput::Reverted(_));
        ensure!(
            reverted == expected.iter().any(|expected| expected.exception),
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
        };
        if let Some(deployment) = deployment
            && !reverted
        {
            self.register_contract(instance, deployment.address, deployment.abi);
        }
        if let Some(Expected::Calldata(expected)) = &step.expected {
            self.assert_calldata(expected, &return_data)?;
        }
        for expected in expected {
            if let Some(data) = &expected.return_data {
                self.assert_calldata(data, &return_data)?;
            }
            if let Some(events) = &expected.events {
                ensure!(
                    events.len() == result.logs.len(),
                    "Unexpected number of contract events"
                );
                for (expected, actual) in events.iter().zip(&result.logs) {
                    if let Some(address) = &expected.address {
                        ensure!(
                            self.address(address)? == actual.address,
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
                        )?;
                    }
                    self.assert_calldata(&expected.values, &actual.data)?;
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
                let name = self.variable_name(name.trim_start_matches("$VARIABLE:"))?;
                self.variables.insert(name, value);
            }
        }
        Ok(())
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
        let execution = self.apply(extrinsic, 1_048_576)?;
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
        let mut dispatch_error = None;
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
                ("Revive", "EthExtrinsicRevert") => dispatch_error = Some(event.field_values()?),
                _ => {}
            }
        }
        let output = match execution.transaction_output {
            Some(output) => output,
            None if dispatch_error.is_none() => {
                bail!("Transaction completed without a captured return value")
            }
            None => bail!("Transaction failed before returning output: {dispatch_error:?}"),
        };
        self.parent = self.api::<RuntimeHeader>("BlockBuilder_finalize_block", &[])?;
        self.transactions.push(TransactionProfilingReport::new(
            transaction_hash,
            caller,
            destination,
            u64::from(nonce),
            started,
            execution.events,
        )?);
        Ok(TransactionResult { output, logs })
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

    fn address(&mut self, address: &StepAddress) -> Result<Address> {
        match address {
            StepAddress::Address(address) => Ok(*address),
            StepAddress::ResolvableAddress(expression) => Ok(Address::from_word(B256::from(
                self.expression(expression)?.to_be_bytes::<32>(),
            ))),
        }
    }

    fn calldata(&mut self, calldata: &Calldata) -> Result<Vec<u8>> {
        match calldata {
            Calldata::Single(bytes) => Ok(bytes.to_vec()),
            Calldata::Compound(items) => items.iter().try_fold(Vec::new(), |mut bytes, item| {
                bytes.extend(self.expression(item.as_ref())?.to_be_bytes::<32>());
                Ok(bytes)
            }),
        }
    }

    fn assert_calldata(&mut self, expected: &Calldata, actual: &[u8]) -> Result<()> {
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
                            self.expression(item.as_ref())? == U256::from_be_bytes(padded),
                            "Return data does not match"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn expression(&mut self, expression: &str) -> Result<U256> {
        let mut stack = Vec::new();
        for token in expression.split_whitespace() {
            let value = match token {
                "+" | "-" | "*" | "/" | "&" | "|" | "^" | "<<" | ">>" => {
                    let right = stack.pop().context("Missing right operand")?;
                    let left: U256 = stack.pop().context("Missing left operand")?;
                    match token {
                        "+" => left.checked_add(right),
                        "-" => left.checked_sub(right),
                        "*" => left.checked_mul(right),
                        "/" => left.checked_div(right),
                        "&" => Some(left & right),
                        "|" => Some(left | right),
                        "^" => Some(left ^ right),
                        "<<" => Some(left << usize::try_from(right)?),
                        ">>" => Some(left >> usize::try_from(right)?),
                        _ => unreachable!(),
                    }
                    .context("Invalid calldata arithmetic")?
                }
                "$CHAIN_ID" => U256::from(self.chain_id),
                "$GAS_LIMIT" => U256::from(
                    self.api::<RuntimeU256>("ReviveApi_block_gas_limit", &[])?
                        .as_u64(),
                ),
                "$BASE_FEE" | "$TRANSACTION_GAS_PRICE" => U256::from(self.gas_price),
                "$BLOCK_NUMBER" => U256::from(*self.parent.number()),
                "$BLOCK_TIMESTAMP" => U256::from(self.timestamp / 1000),
                "$RANDOM_ADDRESS" => U256::from_be_slice(Address::random().as_slice()),
                token => {
                    if let Some(name) = token.strip_suffix(".address") {
                        let name = self.variable_name(name)?;
                        let contract = self
                            .contracts
                            .get(&ContractInstance::new(name))
                            .context("Unknown contract address")?;
                        U256::from_be_slice(contract.address.as_slice())
                    } else if let Some(name) = token.strip_prefix("$VARIABLE:") {
                        let name = self.variable_name(name)?;
                        *self
                            .variables
                            .get(&name)
                            .with_context(|| format!("Variable {name} is undefined"))?
                    } else if let Some(value) = token.strip_prefix('-') {
                        let value = U256::from_str_radix(value, 10)?;
                        ensure!(
                            value > U256::ZERO && value <= U256::ONE << 255,
                            "Invalid negative literal"
                        );
                        U256::MAX - value + U256::ONE
                    } else {
                        U256::from_str_radix(
                            token.trim_start_matches("0x"),
                            if token.starts_with("0x") { 16 } else { 10 },
                        )?
                    }
                }
            };
            stack.push(value);
        }
        match stack.as_slice() {
            [] => Ok(U256::ZERO),
            [value] => Ok(*value),
            _ => bail!("Invalid calldata expression"),
        }
    }

    fn variable_name(&self, name: &str) -> Result<String> {
        let mut resolved = name.to_owned();
        while let Some(start) = resolved.find("$VARIABLE:") {
            let suffix = &resolved[start + 10..];
            let end = suffix
                .find(|character: char| !(character.is_alphanumeric() || character == '_'))
                .unwrap_or(suffix.len());
            let value = self
                .variables
                .get(&suffix[..end])
                .context("Unknown variable in name")?;
            resolved.replace_range(start..start + 10 + end, &value.to_string());
        }
        Ok(resolved)
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

#[derive(Debug, Serialize)]
pub(crate) struct ProfilingReport {
    pub runtime_branch: String,
    pub runtime_commit: String,
    pub workloads: Vec<WorkloadProfilingReport>,
}

impl ProfilingReport {
    pub fn write(&self, working_directory: impl AsRef<Path>) -> Result<PathBuf> {
        let directory = working_directory.as_ref();
        create_dir_all(directory).with_context(|| {
            format!("Failed to create report directory {}", directory.display())
        })?;
        let path = directory.join("profiling_report.json");
        let mut writer =
            BufWriter::new(File::create(&path).with_context(|| {
                format!("Failed to create profiling report {}", path.display())
            })?);
        serde_json::to_writer(&mut writer, self)
            .with_context(|| format!("Failed to write profiling report {}", path.display()))?;
        writer
            .flush()
            .with_context(|| format!("Failed to flush profiling report {}", path.display()))?;
        Ok(path)
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
    pub transactions: Vec<TransactionProfilingReport>,
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
    pub processed_events: Vec<OpCodeProfilingMeasurement>,
    pub raw_events: Vec<RawProfilingEvent>,
}

impl TransactionProfilingReport {
    pub fn new(
        transaction_hash: TxHash,
        sender: Address,
        destination: Option<Address>,
        nonce: u64,
        started: Instant,
        events: ProcessedEventsAndRawEvents,
    ) -> Result<Self> {
        let offset = |instant: Instant| {
            instant
                .checked_duration_since(started)
                .context("Profiling event precedes transaction start")
        };
        let processed_events = events
            .processed_events
            .into_iter()
            .map(|event| {
                Ok(OpCodeProfilingMeasurement {
                    op_code: event.op_code,
                    weight_consumed: event.weight_consumed,
                    started_at: offset(event.instant)?,
                    elapsed: event.elapsed,
                    call_depth: event.call_depth,
                    selector: event.selector,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let raw_events = events
            .raw_events
            .into_iter()
            .map(|event| {
                Ok(match event {
                    ProfilingEvent::OpCodeEnter {
                        op_code,
                        weight_consumed,
                        instant,
                    } => RawProfilingEvent::OpCodeEnter {
                        op_code,
                        weight_consumed,
                        offset: offset(instant)?,
                    },
                    ProfilingEvent::OpCodeExit {
                        op_code,
                        weight_consumed,
                        instant,
                    } => RawProfilingEvent::OpCodeExit {
                        op_code,
                        weight_consumed,
                        offset: offset(instant)?,
                    },
                    ProfilingEvent::CallEnter {
                        op_code,
                        selector,
                        code_address,
                    } => RawProfilingEvent::CallEnter {
                        op_code,
                        selector,
                        code_address,
                    },
                    ProfilingEvent::CallExit { op_code, selector } => {
                        RawProfilingEvent::CallExit { op_code, selector }
                    }
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            transaction_hash,
            sender,
            destination,
            nonce,
            repeat_path: None,
            entry_point: None,
            processed_events,
            raw_events,
        })
    }
}

// Weight and elapsed time include nested calls made by the opcode.
#[derive(Debug, Serialize)]
pub(crate) struct OpCodeProfilingMeasurement {
    pub op_code: u8,
    pub weight_consumed: Weight,
    pub started_at: Duration,
    pub elapsed: Duration,
    pub call_depth: usize,
    pub selector: Option<[u8; 4]>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event")]
pub(crate) enum RawProfilingEvent {
    OpCodeEnter {
        op_code: u8,
        weight_consumed: Weight,
        offset: Duration,
    },
    OpCodeExit {
        op_code: u8,
        weight_consumed: Weight,
        offset: Duration,
    },
    CallEnter {
        op_code: u8,
        selector: Option<[u8; 4]>,
        code_address: RuntimeAddress,
    },
    CallExit {
        op_code: u8,
        selector: Option<[u8; 4]>,
    },
}
