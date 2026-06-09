use crate::internal_prelude::*;

/// A test step.
///
/// A test step can be anything. It could be an invocation to a function, an assertion, or any other
/// action that needs to be run or executed on the nodes used in the tests.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum Step {
    /// A function call or an invocation to some function on some smart contract.
    FunctionCall(Box<FunctionCallStep>),

    /// A step for performing a balance assertion on some account or contract.
    BalanceAssertion(Box<BalanceAssertionStep>),

    /// A step for asserting that the storage of some contract or account is empty.
    StorageEmptyAssertion(Box<StorageEmptyAssertionStep>),

    /// A special step for repeating a bunch of steps a certain number of times.
    Repeat(Box<RepeatStep>),

    /// A step type that allows for a new account address to be allocated and to later on be used
    /// as the caller in another step.
    AllocateAccount(Box<AllocateAccountStep>),

    /// A native value transfer from one account to another.
    Transfer(Box<TransferStep>),
}

/// This is an input step which is a transaction description that the framework translates into a
/// transaction and executes on the nodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct FunctionCallStep {
    /// The address of the account performing the call and paying the fees for it.
    #[serde(default = "FunctionCallStep::default_caller")]
    #[schemars(with = "String")]
    pub caller: StepAddress,

    /// An optional comment on the step which has no impact on the execution in any way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The contract instance that's being called in this transaction step.
    #[serde(default = "FunctionCallStep::default_instance")]
    pub instance: ContractInstanceOrReference<'static>,

    /// The method that's being called in this step.
    pub method: Method,

    /// The calldata that the function should be invoked with.
    #[serde(default)]
    pub calldata: Calldata,

    /// A set of assertions and expectations to have for the transaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Expected>,

    /// An optional value to provide as part of the transaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<EtherValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub storage: Option<HashMap<String, Calldata>>,

    /// Variable assignment to perform in the framework allowing us to reference them again later on
    /// during the execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable_assignments: Option<VariableAssignments>,

    /// Allows for the test to set a specific value for the various gas parameter for each one of
    /// the platforms we support. This is ignored for steps that perform contract deployments.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub gas_overrides: HashMap<PlatformIdentifier, GasOverrides>,
}

/// This represents a balance assertion step where the framework needs to query the balance of some
/// account or contract and assert that it's some amount.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct BalanceAssertionStep {
    /// An optional comment on the balance assertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The address that the balance assertion should be done on.
    ///
    /// This is a string which will be resolved into an address when being processed. Therefore,
    /// this could be a normal hex address, a variable such as `Test.address`, or perhaps even a
    /// full on variable like `$VARIABLE:Uniswap`. It follows the same resolution rules that are
    /// followed in the calldata.
    pub address: StepAddress,

    /// The amount of balance to assert that the account or contract has. This is a 256 bit string
    /// that's serialized and deserialized into a decimal string.
    #[schemars(with = "String")]
    pub expected_balance: U256,
}

/// This represents an assertion for the storage of some contract or account and whether it's empty
/// or not.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct StorageEmptyAssertionStep {
    /// An optional comment on the storage empty assertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The address that the balance assertion should be done on.
    ///
    /// This is a string which will be resolved into an address when being processed. Therefore,
    /// this could be a normal hex address, a variable such as `Test.address`, or perhaps even a
    /// full on variable like `$VARIABLE:Uniswap`. It follows the same resolution rules that are
    /// followed in the calldata.
    pub address: StepAddress,

    /// A boolean of whether the storage of the address is empty or not.
    pub is_storage_empty: bool,
}

/// This represents a repetition step which is a special step type that allows for a sequence of
/// steps to be repeated (on different interpreters) a certain number of times.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct RepeatStep {
    /// An optional comment on the repetition step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// This parameter controls if the interpreter performing the execution should await for the the
    /// transactions within the repeat step to be included before moving on to the next transaction.
    /// By default, this is set to false and it isn't a required parameter to be provided.
    #[serde(default)]
    pub await_transaction_inclusion: bool,

    /// This parameters allows for the index of the repeat step to be captured to a variable to be
    /// referenced later on in the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_index: Option<String>,

    /// This parameter controls if the primary interpreter should consolidate the execution state of the
    /// interpreters that it spawned after running all of them. There are certain cases where this may be
    /// useful such as in cases where the repeat step is being used more as a for loop rather than
    /// anything else.
    #[serde(default)]
    pub consolidate_state: bool,

    /// This parameter controls if the watcher should be started after the warm-up phase of this
    /// repeat step completes. When set to `true`, the interpreter will send the watcher start event
    /// after the first (warm-up) iteration finishes. This should be set on the repeat step that
    /// represents the actual benchmark workload so that blocks produced during setup steps
    /// (e.g., contract deployments) are excluded from the benchmark metrics.
    #[serde(default)]
    pub start_watcher: bool,

    /// The number of repetitions that the steps should be repeated for.
    pub repeat: usize,

    /// The sequence of steps to repeat for the above defined number of repetitions.
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct AllocateAccountStep {
    /// An optional comment on the account allocation step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// An instruction to allocate a new account with the value being the variable name of that
    /// account. This must start with `$VARIABLE:` and then be followed by the variable name of the
    /// account.
    #[serde(rename = "allocate_account")]
    pub variable_name: String,
}

/// A native value transfer step that sends some amount of native currency from one account to
/// another without going through a contract.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct TransferStep {
    /// An optional comment on the transfer step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The sender address.
    #[schemars(with = "String")]
    pub from: StepAddress,

    /// The recipient address.
    #[schemars(with = "String")]
    pub to: StepAddress,

    /// The amount of native currency to transfer.
    pub amount: EtherValue,
}

/// A set of expectations and assertions to make about the transaction after it ran.
///
/// If this is not specified then the only assertion that will be ran is that the transaction
/// was successful.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(untagged)]
pub enum Expected {
    /// An assertion that the transaction succeeded and returned the provided set of data.
    Calldata(Calldata),
    /// A more complex assertion.
    Expected(ExpectedOutput),
    /// A set of assertions.
    ExpectedMany(Vec<ExpectedOutput>),
}

/// A set of assertions to run on the transaction.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
pub struct ExpectedOutput {
    /// An optional compiler version that's required in order for this assertion to run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub compiler_version: Option<VersionReq>,

    /// An optional field of the expected returns from the invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_data: Option<Calldata>,

    /// An optional set of assertions to run on the emitted events from the transaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<Event>>,

    /// A boolean which defines whether we expect the transaction to succeed or fail.
    #[serde(default)]
    pub exception: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
pub struct Event {
    /// An optional field of the address of the emitter of the event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<StepAddress>,

    /// The set of topics to expect the event to have.
    pub topics: Vec<String>,

    /// The set of values to expect the event to have.
    pub values: Calldata,
}

/// A type definition for the calldata supported by the testing framework.
///
/// We choose to document all of the types used in [`Calldata`] in this one doc comment to elaborate
/// on why they exist and consolidate all of the documentation for calldata in a single place where
/// it can be viewed and understood.
///
/// The [`Single`] variant of this enum is quite simple and straightforward: it's a hex-encoded byte
/// array of the calldata.
///
/// The [`Compound`] type is more intricate and allows for capabilities such as resolution and some
/// simple arithmetic operations. It houses a vector of [`CalldataItem`]s which is just a wrapper
/// around an owned string.
///
/// A [`CalldataItem`] could be a simple hex string of a single calldata argument, but it could also
/// be something that requires resolution such as `MyContract.address` which is a variable that is
/// understood by the resolution logic to mean "Lookup the address of this particular contract
/// instance".
///
/// In addition to the above, the format supports some simple arithmetic operations like add, sub,
/// divide, multiply, bitwise AND, bitwise OR, and bitwise XOR. Our parser understands the [reverse
/// polish notation] simply because it's easy to write a calculator for that notation and since we
/// do not have plans to use arithmetic too often in tests. In reverse polish notation a typical
/// `2 + 4` would be written as `2 4 +` which makes this notation very simple to implement through
/// a stack.
///
/// Combining the above, a single [`CalldataItem`] could employ both resolution and arithmetic at
/// the same time. For example, a [`CalldataItem`] of `$BLOCK_NUMBER $BLOCK_NUMBER +` means that
/// the block number should be retrieved and then it should be added to itself.
///
/// Internally, we split the [`CalldataItem`] by spaces. Therefore, `$BLOCK_NUMBER $BLOCK_NUMBER+`
/// is invalid but `$BLOCK_NUMBER $BLOCK_NUMBER +` is valid and can be understood by the parser and
/// calculator. After the split is done, each token is parsed into a [`CalldataToken<&str>`] forming
/// an [`Iterator`] over [`CalldataToken<&str>`]. A [`CalldataToken<&str>`] can then be resolved
/// into a [`CalldataToken<U256>`] through the resolution logic. Finally, after resolution is done,
/// this iterator of [`CalldataToken<U256>`] is collapsed into the final result by applying the
/// arithmetic operations requested.
///
/// For example, supplying a [`Compound`] calldata of `0xdeadbeef` produces an iterator of a single
/// [`CalldataToken<&str>`] items of the value [`CalldataToken::Item`] of the string value 12 which
/// we can then resolve into the appropriate [`U256`] value and convert into calldata.
///
/// In summary, the various types used in [`Calldata`] represent the following:
/// - [`CalldataItem`]: A calldata string from the metadata files.
/// - [`CalldataToken<&str>`]: Typically used in an iterator of items from the space splitted
///   [`CalldataItem`] and represents a token that has not yet been resolved into its value.
/// - [`CalldataToken<U256>`]: Represents a token that's been resolved from being a string and into
///   the word-size calldata argument on which we can perform arithmetic.
///
/// [`Single`]: Calldata::Single
/// [`Compound`]: Calldata::Compound
/// [reverse polish notation]: https://en.wikipedia.org/wiki/Reverse_Polish_notation
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(untagged)]
pub enum Calldata {
    Single(#[schemars(with = "String")] Bytes),
    Compound(Vec<CalldataItem>),
}

define_wrapper_type! {
    /// This represents an item in the [`Calldata::Compound`] variant. Each item will be resolved
    /// according to the resolution rules of the tool.
    #[rustfmt::skip]
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, Display)]
    #[serde(transparent)]
    pub struct CalldataItem(String);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CalldataToken<T> {
    Item(T),
    Operation(Operation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Operation {
    Addition,
    Subtraction,
    Multiplication,
    Division,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    ShiftLeft,
    ShiftRight,
}

/// Specify how the contract is called.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema, Clone, Eq, PartialEq)]
pub enum Method {
    /// Initiate a deploy transaction, calling contracts constructor.
    ///
    /// Indicated by `#deployer`.
    #[serde(rename = "#deployer")]
    Deployer,

    /// Does not calculate and insert a function selector.
    ///
    /// Indicated by `#fallback`.
    #[default]
    #[serde(rename = "#fallback")]
    Fallback,

    /// Call the public function with the given name.
    #[serde(untagged)]
    FunctionName(String),
}

define_wrapper_type!(
    /// Defines an Ether value.
    ///
    /// This is an unsigned 256 bit integer that's followed by some denomination which can either be
    /// eth, ether, gwei, or wei.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
    #[schemars(with = "String")]
    pub struct EtherValue(U256) impl Display;
);

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct VariableAssignments {
    /// A vector of the variable names to assign to the return data.
    ///
    /// Example: `UniswapV3PoolAddress`
    pub return_data: Vec<String>,
}

/// An address type that might either be an address literal or a resolvable address.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, JsonSchema, Display)]
#[schemars(with = "String")]
#[serde(untagged)]
pub enum StepAddress {
    Address(Address),
    ResolvableAddress(String),
}

impl Default for StepAddress {
    fn default() -> Self {
        Self::Address(Default::default())
    }
}

impl StepAddress {
    pub fn as_address(&self) -> Option<&Address> {
        match self {
            StepAddress::Address(address) => Some(address),
            StepAddress::ResolvableAddress(_) => None,
        }
    }

    pub async fn resolve_address<R: LazyResolverApi>(
        &self,
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<Address> {
        match self {
            StepAddress::Address(address) => Ok(*address),
            StepAddress::ResolvableAddress(address) => Ok(Address::from_slice(
                Calldata::new_compound([address])
                    .calldata(context)
                    .await?
                    .get(12..32)
                    .expect("Can't fail"),
            )),
        }
    }
}

impl FunctionCallStep {
    pub const fn default_caller_address() -> Address {
        Address(FixedBytes(alloy::hex!(
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1"
        )))
    }

    pub const fn default_caller() -> StepAddress {
        StepAddress::Address(Self::default_caller_address())
    }

    fn default_instance() -> ContractInstanceOrReference<'static> {
        ContractInstance::new("Test").into()
    }
}

impl ExpectedOutput {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_success(mut self) -> Self {
        self.exception = false;
        self
    }

    pub fn with_calldata(mut self, calldata: Calldata) -> Self {
        self.return_data = Some(calldata);
        self
    }
}

impl Default for Calldata {
    fn default() -> Self {
        Self::Compound(Default::default())
    }
}

impl Calldata {
    pub fn new_compound(items: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self::Compound(
            items
                .into_iter()
                .map(|item| item.as_ref().to_owned())
                .map(CalldataItem::new)
                .collect(),
        )
    }

    pub async fn calldata<R: LazyResolverApi>(
        &self,
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut buffer = Vec::<u8>::with_capacity(self.size_requirement());
        self.calldata_into_slice(&mut buffer, context).await?;
        Ok(buffer)
    }

    pub async fn calldata_into_slice<R: LazyResolverApi>(
        &self,
        buffer: &mut Vec<u8>,
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<()> {
        match self {
            Calldata::Single(bytes) => {
                buffer.extend_from_slice(bytes);
            }
            Calldata::Compound(items) => {
                for (arg_idx, arg) in items.iter().enumerate() {
                    let resolved = arg
                        .resolve(context)
                        .instrument(info_span!("Resolving argument", %arg, arg_idx))
                        .map_ok(|value| value.to_be_bytes::<32>())
                        .await?;
                    buffer.extend(resolved);
                }
            }
        };
        Ok(())
    }

    pub fn size_requirement(&self) -> usize {
        match self {
            Calldata::Single(single) => single.len(),
            Calldata::Compound(items) => items.len() * 32,
        }
    }

    pub async fn is_equivalent<R: LazyResolverApi>(
        &self,
        other: &[u8],
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<bool> {
        match self {
            Calldata::Single(calldata) => Ok(calldata == other),
            Calldata::Compound(items) => {
                for (expected, actual) in items.iter().zip(other.chunks(32)) {
                    if expected.as_ref() == "*" {
                        continue;
                    }

                    let actual = if actual.len() < 32 {
                        let mut vec = actual.to_vec();
                        vec.resize(32, 0);
                        std::borrow::Cow::Owned(vec)
                    } else {
                        std::borrow::Cow::Borrowed(actual)
                    };

                    let expected = expected
                        .resolve(context)
                        .await
                        .context("Resolution failed")?;
                    let actual = U256::from_be_slice(&actual);
                    anyhow::ensure!(
                        expected == actual,
                        "Two parts of the equivalence check aren't equivalent. Expected = {}, actual = {}",
                        expected,
                        actual
                    )
                }
                Ok(true)
            }
        }
    }
}

impl CalldataItem {
    #[instrument(level = "info", skip_all, err(Debug))]
    async fn resolve<R: LazyResolverApi>(
        &self,
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<U256> {
        let mut stack = Vec::<CalldataToken<U256>>::new();

        for token in self.calldata_tokens() {
            let token = token.resolve(context).await?;
            let new_token = match token {
                CalldataToken::Item(_) => token,
                CalldataToken::Operation(operation) => {
                    let right_operand = stack
                        .pop()
                        .and_then(CalldataToken::into_item)
                        .context("Invalid calldata arithmetic operation")?;
                    let left_operand = stack
                        .pop()
                        .and_then(CalldataToken::into_item)
                        .context("Invalid calldata arithmetic operation")?;

                    let result = match operation {
                        Operation::Addition => left_operand.checked_add(right_operand),
                        Operation::Subtraction => left_operand.checked_sub(right_operand),
                        Operation::Multiplication => left_operand.checked_mul(right_operand),
                        Operation::Division => left_operand.checked_div(right_operand),
                        Operation::BitwiseAnd => Some(left_operand & right_operand),
                        Operation::BitwiseOr => Some(left_operand | right_operand),
                        Operation::BitwiseXor => Some(left_operand ^ right_operand),
                        Operation::ShiftLeft => {
                            Some(left_operand << usize::try_from(right_operand)?)
                        }
                        Operation::ShiftRight => {
                            Some(left_operand >> usize::try_from(right_operand)?)
                        }
                    }
                    .context("Invalid calldata arithmetic operation - Invalid operation")?;

                    CalldataToken::Item(result)
                }
            };
            stack.push(new_token)
        }

        match stack.as_slice() {
            // Empty stack means that we got an empty compound calldata which we resolve to zero.
            [] => Ok(U256::ZERO),
            [CalldataToken::Item(item)] => {
                tracing::debug!(
                    original_item = ?self,
                    resolved_item = item.to_be_bytes::<32>().encode_hex(),
                    "Resolution Done"
                );
                Ok(*item)
            }
            _ => Err(anyhow::anyhow!(
                "Invalid calldata arithmetic operation - Invalid stack"
            )),
        }
    }

    fn calldata_tokens(&self) -> impl Iterator<Item = CalldataToken<&str>> {
        self.0.split(' ').map(|item| match item {
            "+" => CalldataToken::Operation(Operation::Addition),
            "-" => CalldataToken::Operation(Operation::Subtraction),
            "/" => CalldataToken::Operation(Operation::Division),
            "*" => CalldataToken::Operation(Operation::Multiplication),
            "&" => CalldataToken::Operation(Operation::BitwiseAnd),
            "|" => CalldataToken::Operation(Operation::BitwiseOr),
            "^" => CalldataToken::Operation(Operation::BitwiseXor),
            "<<" => CalldataToken::Operation(Operation::ShiftLeft),
            ">>" => CalldataToken::Operation(Operation::ShiftRight),
            _ => CalldataToken::Item(item),
        })
    }
}

impl<T> CalldataToken<T> {
    const ADDRESS_VARIABLE_SUFFIX: &str = ".address";
    const NEGATIVE_VALUE_PREFIX: char = '-';
    const HEX_LITERAL_PREFIX: &str = "0x";
    const CHAIN_VARIABLE: &str = "$CHAIN_ID";
    const GAS_LIMIT_VARIABLE: &str = "$GAS_LIMIT";
    const COINBASE_VARIABLE: &str = "$COINBASE";
    const DIFFICULTY_VARIABLE: &str = "$DIFFICULTY";
    const BLOCK_BASE_FEE_VARIABLE: &str = "$BASE_FEE";
    const BLOCK_HASH_VARIABLE_PREFIX: &str = "$BLOCK_HASH";
    const BLOCK_NUMBER_VARIABLE: &str = "$BLOCK_NUMBER";
    const BLOCK_TIMESTAMP_VARIABLE: &str = "$BLOCK_TIMESTAMP";
    const TRANSACTION_GAS_PRICE: &str = "$TRANSACTION_GAS_PRICE";
    const RANDOM_ADDRESS_VARIABLE: &str = "$RANDOM_ADDRESS";
    const VARIABLE_PREFIX: &str = "$VARIABLE:";

    fn into_item(self) -> Option<T> {
        match self {
            CalldataToken::Item(item) => Some(item),
            CalldataToken::Operation(_) => None,
        }
    }
}

fn required_node_connector<'a, R: LazyResolverApi>(
    context: &mut ResolutionContext<'a, R>,
) -> anyhow::Result<Arc<NodeConnector>> {
    context
        .node_connector()
        .context("No node connector provided")
}

async fn resolve_latest_block<R: LazyResolverApi>(
    context: &mut ResolutionContext<'_, R>,
) -> anyhow::Result<BlockPair> {
    match context.pinned_block() {
        Some(block) => Ok(block.clone()),
        None => required_node_connector(context)?
            .latest_finalized_block()
            .await
            .context("Failed to query latest finalized block"),
    }
}

async fn resolve_current_block_number<R: LazyResolverApi>(
    context: &mut ResolutionContext<'_, R>,
) -> anyhow::Result<u64> {
    match context.pinned_block() {
        Some(block) => Ok(block.evm_block.number()),
        None => required_node_connector(context)?
            .latest_finalized_block()
            .await
            .context("Failed to query latest finalized block")
            .map(|block| block.evm_block.number()),
    }
}

impl<T: AsRef<str>> CalldataToken<T> {
    /// This function takes in the string calldata argument provided in the JSON input and resolves
    /// it into a [`U256`] which is later used to construct the calldata.
    ///
    /// # Note
    ///
    /// This piece of code is taken from the matter-labs-tester repository which is licensed under
    /// MIT or Apache. The original source code can be found here:
    /// https://github.com/matter-labs/era-compiler-tester/blob/0ed598a27f6eceee7008deab3ff2311075a2ec69/compiler_tester/src/test/case/input/value.rs#L43-L146
    pub async fn resolve<R: LazyResolverApi>(
        self,
        context: &mut ResolutionContext<'_, R>,
    ) -> anyhow::Result<CalldataToken<U256>> {
        match self {
            Self::Item(item) => {
                let item = item.as_ref();
                let value = if let Some(instance) = item.strip_suffix(Self::ADDRESS_VARIABLE_SUFFIX)
                {
                    let contract_ref = ContractInstanceOrReference::from(instance.to_owned());
                    context
                        .get_contract_address(&contract_ref)
                        .await
                        .ok_or_else(|| anyhow::anyhow!("Instance `{}` not found", instance))
                        .and_then(|address| {
                            address.map(|address| U256::from_be_slice(address.as_ref()))
                        })
                } else if let Some(value) = item.strip_prefix(Self::NEGATIVE_VALUE_PREFIX) {
                    let value = U256::from_str_radix(value, 10).map_err(|error| {
                        anyhow::anyhow!("Invalid decimal literal after `-`: {}", error)
                    })?;
                    if value > U256::ONE << 255u8 {
                        anyhow::bail!("Decimal literal after `-` is too big");
                    }
                    let value = value
                        .checked_sub(U256::ONE)
                        .ok_or_else(|| anyhow::anyhow!("`-0` is invalid literal"))?;
                    Ok(U256::MAX.checked_sub(value).expect("Always valid"))
                } else if let Some(value) = item.strip_prefix(Self::HEX_LITERAL_PREFIX) {
                    U256::from_str_radix(value, 16)
                        .map_err(|error| anyhow::anyhow!("Invalid hexadecimal literal: {}", error))
                } else if item == Self::CHAIN_VARIABLE {
                    required_node_connector(context)?
                        .chain_id()
                        .await
                        .map(U256::from)
                } else if item == Self::TRANSACTION_GAS_PRICE {
                    let tx_hash = context
                        .transaction_hash()
                        .context("No transaction hash provided to get the transaction gas price")?;
                    required_node_connector(context)?
                        .get_receipt(*tx_hash)
                        .await
                        .context("Failed to get the transaction receipt")
                        .map(|receipt| U256::from(receipt.effective_gas_price))
                } else if item == Self::GAS_LIMIT_VARIABLE {
                    resolve_latest_block(context)
                        .await
                        .map(|block| U256::from(block.evm_block.header.gas_limit))
                } else if item == Self::COINBASE_VARIABLE {
                    resolve_latest_block(context).await.map(|block| {
                        U256::from_be_slice(block.evm_block.header.beneficiary.as_ref())
                    })
                } else if item == Self::DIFFICULTY_VARIABLE {
                    resolve_latest_block(context)
                        .await
                        .map(|block| U256::from_be_bytes(block.evm_block.header.mix_hash.0))
                } else if item == Self::BLOCK_BASE_FEE_VARIABLE {
                    resolve_latest_block(context)
                        .await?
                        .evm_block
                        .header
                        .base_fee_per_gas
                        .context("Failed to get the base fee per gas")
                        .map(U256::from)
                } else if item.starts_with(Self::BLOCK_HASH_VARIABLE_PREFIX) {
                    let offset = item
                        .split(':')
                        .next_back()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_default();

                    let current_block_number = resolve_current_block_number(context)
                        .await
                        .context("Failed to query last block number while resolving $BLOCK_HASH")?;
                    let desired_block_number = current_block_number.saturating_sub(offset);

                    if let Some(block) = context
                        .pinned_block()
                        .filter(|block| block.evm_block.number() == desired_block_number)
                    {
                        Ok(U256::from_be_bytes(block.evm_block.header.hash.0))
                    } else {
                        Ok(U256::from_be_bytes(
                            required_node_connector(context)?
                                .block(desired_block_number)
                                .await
                                .evm_block
                                .header
                                .hash
                                .0,
                        ))
                    }
                } else if item == Self::BLOCK_NUMBER_VARIABLE {
                    let current_block_number =
                        resolve_current_block_number(context).await.context(
                            "Failed to query last block number while resolving $BLOCK_NUMBER",
                        )?;
                    Ok(U256::from(current_block_number))
                } else if item == Self::BLOCK_TIMESTAMP_VARIABLE {
                    resolve_latest_block(context)
                        .await
                        .map(|block| U256::from(block.evm_block.header.timestamp))
                } else if item == Self::RANDOM_ADDRESS_VARIABLE {
                    Ok(U256::from_be_slice(Address::random().as_ref()))
                } else if let Some(variable_name) = item.strip_prefix(Self::VARIABLE_PREFIX) {
                    context
                        .get_variable(variable_name)
                        .await
                        .context("Variable lookup failed")
                        .and_then(|value| value)
                } else {
                    U256::from_str_radix(item, 10)
                        .map_err(|error| anyhow::anyhow!("Invalid decimal literal: {}", error))
                };
                value.map(CalldataToken::Item)
            }
            Self::Operation(operation) => Ok(CalldataToken::Operation(operation)),
        }
    }
}

impl Serialize for EtherValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        format!("{} wei", self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EtherValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string = String::deserialize(deserializer)?;
        let mut splitted = string.split(' ');
        let (Some(value), Some(unit)) = (splitted.next(), splitted.next()) else {
            return Err(serde::de::Error::custom("Failed to parse the value"));
        };
        let parsed = parse_units(value, unit.replace("eth", "ether"))
            .map_err(|_| serde::de::Error::custom("Failed to parse units"))?
            .into();
        Ok(Self(parsed))
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct GasOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_limit: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_price: Option<u128>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fee_per_gas: Option<u128>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_priority_fee_per_gas: Option<u128>,
}

impl GasOverrides {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn apply_to<N: Network>(&self, transaction_request: &mut N::TransactionRequest) {
        if let Some(gas_limit) = self.gas_limit {
            transaction_request.set_gas_limit(gas_limit);
        }
        if let Some(gas_price) = self.gas_price {
            transaction_request.set_gas_price(gas_price);
        }
        if let Some(max_fee_per_gas) = self.max_fee_per_gas {
            transaction_request.set_max_fee_per_gas(max_fee_per_gas);
        }
        if let Some(max_priority_fee_per_gas) = self.max_priority_fee_per_gas {
            transaction_request.set_max_priority_fee_per_gas(max_priority_fee_per_gas)
        }
    }
}

#[cfg(test)]
mod tests {

    use alloy::json_abi::JsonAbi;
    use alloy::primitives::address;
    use alloy::sol_types::SolValue;
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct MockResolver {
        contract_addresses: HashMap<ContractInstance, Address>,
        variables: HashMap<String, U256>,
    }

    impl MockResolver {
        fn context(&mut self) -> ResolutionContext<'_, Self> {
            ResolutionContext {
                metadata: None,
                pinned_block: None,
                transaction_hash: None,
                node_connector: None,
                api: Some(self),
            }
        }

        fn with_contract(mut self, instance: ContractInstance, address: Address) -> Self {
            self.contract_addresses.insert(instance, address);
            self
        }

        fn with_variable(mut self, variable: impl Into<String>, value: U256) -> Self {
            self.variables.insert(variable.into(), value);
            self
        }
    }

    impl LazyResolverApi for MockResolver {
        async fn get_contract_address(
            &mut self,
            contract_ref: &ContractInstanceOrReference<'_>,
        ) -> anyhow::Result<Address> {
            match contract_ref {
                ContractInstanceOrReference::Instance(instance) => self
                    .contract_addresses
                    .get(instance.as_ref())
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("Instance `{}` not found", instance)),
                ContractInstanceOrReference::Reference(reference) => {
                    anyhow::bail!("Reference `{}` not supported by mock resolver", reference)
                }
            }
        }

        async fn get_variable(
            &mut self,
            variable: impl AsRef<str>,
        ) -> Option<anyhow::Result<U256>> {
            self.variables.get(variable.as_ref()).copied().map(Ok)
        }
    }

    async fn calldata_with_selector(
        calldata: &Calldata,
        selector: [u8; 4],
        resolver: &mut MockResolver,
    ) -> anyhow::Result<Vec<u8>> {
        let mut encoded = Vec::<u8>::with_capacity(4 + calldata.size_requirement());
        encoded.extend(selector);
        let mut context = resolver.context();
        encoded.extend(calldata.calldata(&mut context).await?);
        Ok(encoded)
    }

    #[tokio::test]
    async fn test_encoded_input_uint256() {
        // Arrange
        let raw_metadata = r#"
            [
                {
                    "inputs": [{"name": "value", "type": "uint256"}],
                    "name": "store",
                    "outputs": [],
                    "stateMutability": "nonpayable",
                    "type": "function"
                }
            ]
        "#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_metadata).unwrap();
        let selector = parsed_abi
            .function("store")
            .unwrap()
            .first()
            .unwrap()
            .selector()
            .0;

        let calldata = Calldata::new_compound(["42"]);
        let mut resolver = MockResolver::default();

        // Act
        let encoded = calldata_with_selector(&calldata, selector, &mut resolver)
            .await
            .unwrap();

        // Assert
        assert!(encoded.starts_with(&selector));

        type T = (u64,);
        let decoded: T = T::abi_decode(&encoded[4..]).unwrap();
        assert_eq!(decoded.0, 42);
    }

    #[tokio::test]
    async fn test_encoded_input_address_with_signature() {
        // Arrange
        let raw_abi = r#"[
        {
            "inputs": [{"name": "recipient", "type": "address"}],
            "name": "send",
            "outputs": [],
            "stateMutability": "nonpayable",
            "type": "function"
        }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = parsed_abi
            .function("send")
            .unwrap()
            .first()
            .unwrap()
            .selector()
            .0;

        let calldata = Calldata::new_compound(["Contract.address"]);
        let mut resolver = MockResolver::default().with_contract(
            ContractInstance::new("Contract"),
            address!("0x1000000000000000000000000000000000000001"),
        );

        // Act
        let encoded = calldata_with_selector(&calldata, selector, &mut resolver)
            .await
            .unwrap();

        // Assert
        assert!(encoded.starts_with(&selector));

        type T = (alloy::primitives::Address,);
        let decoded: T = T::abi_decode(&encoded[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }

    #[tokio::test]
    async fn test_encoded_input_address() {
        // Arrange
        let raw_abi = r#"[
        {
            "inputs": [{"name": "recipient", "type": "address"}],
            "name": "send",
            "outputs": [],
            "stateMutability": "nonpayable",
            "type": "function"
        }
        ]"#;

        let parsed_abi: JsonAbi = serde_json::from_str(raw_abi).unwrap();
        let selector = parsed_abi
            .function("send")
            .unwrap()
            .first()
            .unwrap()
            .selector()
            .0;

        let calldata = Calldata::new_compound(["0x1000000000000000000000000000000000000001"]);
        let mut resolver = MockResolver::default();

        // Act
        let encoded = calldata_with_selector(&calldata, selector, &mut resolver)
            .await
            .unwrap();

        // Assert
        assert!(encoded.starts_with(&selector));

        type T = (alloy::primitives::Address,);
        let decoded: T = T::abi_decode(&encoded[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }

    async fn resolve_calldata_item(
        input: &str,
        resolver: &mut MockResolver,
    ) -> anyhow::Result<U256> {
        let mut context = resolver.context();
        CalldataItem::new(input).resolve(&mut context).await
    }

    #[tokio::test]
    async fn simple_addition_can_be_resolved() {
        // Arrange
        let input = "2 4 +";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(6));
    }

    #[tokio::test]
    async fn simple_subtraction_can_be_resolved() {
        // Arrange
        let input = "4 2 -";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(2));
    }

    #[tokio::test]
    async fn simple_multiplication_can_be_resolved() {
        // Arrange
        let input = "4 2 *";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(8));
    }

    #[tokio::test]
    async fn simple_division_can_be_resolved() {
        // Arrange
        let input = "4 2 /";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(2));
    }

    #[tokio::test]
    async fn arithmetic_errors_are_not_panics() {
        // Arrange
        let input = "4 0 /";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        assert!(resolved.is_err())
    }

    #[tokio::test]
    async fn arithmetic_with_resolution_works() {
        // Arrange
        let input = "$VARIABLE:BLOCK_NUMBER 10 +";
        let mut resolver = MockResolver::default().with_variable("BLOCK_NUMBER", U256::from(10));

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(20));
    }

    #[tokio::test]
    async fn incorrect_number_of_arguments_errors() {
        // Arrange
        let input = "1 10 + +";
        let mut resolver = MockResolver::default();

        // Act
        let resolved = resolve_calldata_item(input, &mut resolver).await;

        // Assert
        assert!(resolved.is_err())
    }

    #[test]
    fn expected_json_can_be_deserialized1() {
        // Arrange
        let str = r#"
        {
            "return_data": [
                "1"
            ],
            "events": [
                {
                    "topics": [],
                    "values": []
                }
            ]
        }
        "#;

        // Act
        let expected = serde_json::from_str::<Expected>(str);

        // Assert
        expected.expect("Failed to deserialize");
    }

    #[test]
    fn expected_json_can_be_deserialized2() {
        // Arrange
        let str = r#"
        {
            "return_data": [
                "1"
            ],
            "events": [
                {
                    "address": "Main.address",
                    "topics": [],
                    "values": []
                }
            ]
        }
        "#;

        // Act
        let expected = serde_json::from_str::<Expected>(str);

        // Assert
        expected.expect("Failed to deserialize");
    }
}
