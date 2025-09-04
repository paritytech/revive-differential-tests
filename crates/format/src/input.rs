use std::collections::HashMap;

use alloy::{
    eips::BlockNumberOrTag,
    json_abi::Function,
    network::TransactionBuilder,
    primitives::{Address, Bytes, U256},
    rpc::types::TransactionRequest,
};
use alloy_primitives::{FixedBytes, utils::parse_units};
use anyhow::Context as _;
use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt, stream};
use semver::VersionReq;
use serde::{Deserialize, Serialize};

use revive_dt_common::macros::define_wrapper_type;
use tracing::{Instrument, info_span, instrument};

use crate::traits::ResolverApi;
use crate::{metadata::ContractInstance, traits::ResolutionContext};

/// A test step.
///
/// A test step can be anything. It could be an invocation to a function, an assertion, or any other
/// action that needs to be run or executed on the nodes used in the tests.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Step {
    /// A function call or an invocation to some function on some smart contract.
    FunctionCall(Box<Input>),
    /// A step for performing a balance assertion on some account or contract.
    BalanceAssertion(Box<BalanceAssertion>),
    /// A step for asserting that the storage of some contract or account is empty.
    StorageEmptyAssertion(Box<StorageEmptyAssertion>),
}

define_wrapper_type!(
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct StepIdx(usize) impl Display;
);

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct Input {
    #[serde(default = "Input::default_caller")]
    pub caller: Address,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    #[serde(default = "Input::default_instance")]
    pub instance: ContractInstance,

    pub method: Method,

    #[serde(default)]
    pub calldata: Calldata,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Expected>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<EtherValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<HashMap<String, Calldata>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable_assignments: Option<VariableAssignments>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct BalanceAssertion {
    /// An optional comment on the balance assertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The address that the balance assertion should be done on.
    ///
    /// This is a string which will be resolved into an address when being processed. Therefore,
    /// this could be a normal hex address, a variable such as `Test.address`, or perhaps even a
    /// full on variable like `$VARIABLE:Uniswap`. It follows the same resolution rules that are
    /// followed in the calldata.
    pub address: String,

    /// The amount of balance to assert that the account or contract has.
    pub expected_balance: U256,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct StorageEmptyAssertion {
    /// An optional comment on the storage empty assertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// The address that the balance assertion should be done on.
    ///
    /// This is a string which will be resolved into an address when being processed. Therefore,
    /// this could be a normal hex address, a variable such as `Test.address`, or perhaps even a
    /// full on variable like `$VARIABLE:Uniswap`. It follows the same resolution rules that are
    /// followed in the calldata.
    pub address: String,

    /// A boolean of whether the storage of the address is empty or not.
    pub is_storage_empty: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Expected {
    Calldata(Calldata),
    Expected(ExpectedOutput),
    ExpectedMany(Vec<ExpectedOutput>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct ExpectedOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<VersionReq>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_data: Option<Calldata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<Event>>,
    #[serde(default)]
    pub exception: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct Event {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub topics: Vec<String>,
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
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum Calldata {
    Single(Bytes),
    Compound(Vec<CalldataItem>),
}

define_wrapper_type! {
    /// This represents an item in the [`Calldata::Compound`] variant.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct CalldataItem(String) impl Display;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
enum CalldataToken<T> {
    Item(T),
    Operation(Operation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
enum Operation {
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
#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq)]
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
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct EtherValue(U256) impl Display;
);

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct VariableAssignments {
    /// A vector of the variable names to assign to the return data.
    ///
    /// Example: `UniswapV3PoolAddress`
    pub return_data: Vec<String>,
}

impl Input {
    pub const fn default_caller() -> Address {
        Address(FixedBytes(alloy::hex!(
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1"
        )))
    }

    fn default_instance() -> ContractInstance {
        ContractInstance::new("Test")
    }

    pub async fn encoded_input(
        &self,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<Bytes> {
        match self.method {
            Method::Deployer | Method::Fallback => {
                let calldata = self
                    .calldata
                    .calldata(resolver, context)
                    .await
                    .context("Failed to produce calldata for deployer/fallback method")?;

                Ok(calldata.into())
            }
            Method::FunctionName(ref function_name) => {
                let Some(abi) = context.deployed_contract_abi(&self.instance) else {
                    anyhow::bail!("ABI for instance '{}' not found", self.instance.as_ref());
                };

                // We follow the same logic that's implemented in the matter-labs-tester where they resolve
                // the function name into a function selector and they assume that he function doesn't have
                // any existing overloads.
                // Overloads are handled by providing the full function signature in the "function
                // name".
                // https://github.com/matter-labs/era-compiler-tester/blob/1dfa7d07cba0734ca97e24704f12dd57f6990c2c/compiler_tester/src/test/case/input/mod.rs#L158-L190
                let selector =
                    if function_name.contains('(') && function_name.contains(')') {
                        Function::parse(function_name)
                        .context(
                            "Failed to parse the provided function name into a function signature",
                        )?
                        .selector()
                    } else {
                        abi.functions()
                        .find(|function| function.signature().starts_with(function_name))
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Function with name {:?} not found in ABI for the instance {:?}",
                                function_name,
                                &self.instance
                            )
                        })
                        .with_context(|| format!(
                            "Failed to resolve function selector for {:?} on instance {:?}",
                            function_name, &self.instance
                        ))?
                        .selector()
                    };

                // Allocating a vector that we will be using for the calldata. The vector size will be:
                // 4 bytes for the function selector.
                // function.inputs.len() * 32 bytes for the arguments (each argument is a U256).
                //
                // We're using indices in the following code in order to avoid the need for us to allocate
                // a new buffer for each one of the resolved arguments.
                let mut calldata = Vec::<u8>::with_capacity(4 + self.calldata.size_requirement());
                calldata.extend(selector.0);
                self.calldata
                    .calldata_into_slice(&mut calldata, resolver, context)
                    .await
                    .context("Failed to append encoded argument to calldata buffer")?;

                Ok(calldata.into())
            }
        }
    }

    /// Parse this input into a legacy transaction.
    pub async fn legacy_transaction(
        &self,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<TransactionRequest> {
        let input_data = self
            .encoded_input(resolver, context)
            .await
            .context("Failed to encode input bytes for transaction request")?;
        let transaction_request = TransactionRequest::default().from(self.caller).value(
            self.value
                .map(|value| value.into_inner())
                .unwrap_or_default(),
        );
        match self.method {
            Method::Deployer => Ok(transaction_request.with_deploy_code(input_data)),
            _ => Ok(transaction_request
                .to(context
                    .deployed_contract_address(&self.instance)
                    .context("Failed to get the contract address")
                    .copied()?)
                .input(input_data.into())),
        }
    }

    pub fn find_all_contract_instances(&self) -> Vec<ContractInstance> {
        let mut vec = Vec::new();
        vec.push(self.instance.clone());

        self.calldata.find_all_contract_instances(&mut vec);

        vec
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

    pub fn with_failure(mut self) -> Self {
        self.exception = true;
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
    pub fn new_single(item: impl Into<Bytes>) -> Self {
        Self::Single(item.into())
    }

    pub fn new_compound(items: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self::Compound(
            items
                .into_iter()
                .map(|item| item.as_ref().to_owned())
                .map(CalldataItem::new)
                .collect(),
        )
    }

    pub fn find_all_contract_instances(&self, vec: &mut Vec<ContractInstance>) {
        if let Calldata::Compound(compound) = self {
            for item in compound {
                if let Some(instance) =
                    item.strip_suffix(CalldataToken::<()>::ADDRESS_VARIABLE_SUFFIX)
                {
                    vec.push(ContractInstance::new(instance))
                }
            }
        }
    }

    pub async fn calldata(
        &self,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut buffer = Vec::<u8>::with_capacity(self.size_requirement());
        self.calldata_into_slice(&mut buffer, resolver, context)
            .await?;
        Ok(buffer)
    }

    pub async fn calldata_into_slice(
        &self,
        buffer: &mut Vec<u8>,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<()> {
        match self {
            Calldata::Single(bytes) => {
                buffer.extend_from_slice(bytes);
            }
            Calldata::Compound(items) => {
                let resolved = stream::iter(items.iter().enumerate())
                    .map(|(arg_idx, arg)| async move {
                        arg.resolve(resolver, context)
                            .instrument(info_span!("Resolving argument", %arg, arg_idx))
                            .map_ok(|value| value.to_be_bytes::<32>())
                            .await
                    })
                    .buffered(0xFF)
                    .try_collect::<Vec<_>>()
                    .await
                    .context("Failed to resolve one or more calldata arguments")?;

                buffer.extend(resolved.into_iter().flatten());
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

    /// Checks if this [`Calldata`] is equivalent to the passed calldata bytes.
    pub async fn is_equivalent(
        &self,
        other: &[u8],
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<bool> {
        match self {
            Calldata::Single(calldata) => Ok(calldata == other),
            Calldata::Compound(items) => {
                stream::iter(items.iter().zip(other.chunks(32)))
                    .map(|(this, other)| async move {
                        // The matterlabs format supports wildcards and therefore we
                        // also need to support them.
                        if this.as_ref() == "*" {
                            return Ok::<_, anyhow::Error>(true);
                        }

                        let other = if other.len() < 32 {
                            let mut vec = other.to_vec();
                            vec.resize(32, 0);
                            std::borrow::Cow::Owned(vec)
                        } else {
                            std::borrow::Cow::Borrowed(other)
                        };

                        let this = this
                            .resolve(resolver, context)
                            .await
                            .context("Failed to resolve calldata item during equivalence check")?;
                        let other = U256::from_be_slice(&other);
                        Ok(this == other)
                    })
                    .buffered(0xFF)
                    .all(|v| async move { v.is_ok_and(|v| v) })
                    .map(Ok)
                    .await
            }
        }
    }
}

impl CalldataItem {
    #[instrument(level = "info", skip_all, err)]
    async fn resolve(
        &self,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<U256> {
        let mut stack = Vec::<CalldataToken<U256>>::new();

        for token in self
            .calldata_tokens()
            .map(|token| token.resolve(resolver, context))
        {
            let token = token.await?;
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
            [CalldataToken::Item(item)] => Ok(*item),
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
    const VARIABLE_PREFIX: &str = "$VARIABLE:";

    fn into_item(self) -> Option<T> {
        match self {
            CalldataToken::Item(item) => Some(item),
            CalldataToken::Operation(_) => None,
        }
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
    async fn resolve(
        self,
        resolver: &impl ResolverApi,
        context: ResolutionContext<'_>,
    ) -> anyhow::Result<CalldataToken<U256>> {
        match self {
            Self::Item(item) => {
                let item = item.as_ref();
                let value = if let Some(instance) = item.strip_suffix(Self::ADDRESS_VARIABLE_SUFFIX)
                {
                    context
                        .deployed_contract_address(&ContractInstance::new(instance))
                        .ok_or_else(|| anyhow::anyhow!("Instance `{}` not found", instance))
                        .map(AsRef::as_ref)
                        .map(U256::from_be_slice)
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
                    resolver.chain_id().await.map(U256::from)
                } else if item == Self::TRANSACTION_GAS_PRICE {
                    context
                        .transaction_hash()
                        .context("No transaction hash provided to get the transaction gas price")
                        .map(|tx_hash| resolver.transaction_gas_price(tx_hash))?
                        .await
                        .map(U256::from)
                } else if item == Self::GAS_LIMIT_VARIABLE {
                    resolver
                        .block_gas_limit(context.resolve_block_number(BlockNumberOrTag::Latest))
                        .await
                        .map(U256::from)
                } else if item == Self::COINBASE_VARIABLE {
                    resolver
                        .block_coinbase(context.resolve_block_number(BlockNumberOrTag::Latest))
                        .await
                        .map(|address| U256::from_be_slice(address.as_ref()))
                } else if item == Self::DIFFICULTY_VARIABLE {
                    resolver
                        .block_difficulty(context.resolve_block_number(BlockNumberOrTag::Latest))
                        .await
                } else if item == Self::BLOCK_BASE_FEE_VARIABLE {
                    resolver
                        .block_base_fee(context.resolve_block_number(BlockNumberOrTag::Latest))
                        .await
                        .map(U256::from)
                } else if item.starts_with(Self::BLOCK_HASH_VARIABLE_PREFIX) {
                    let offset: u64 = item
                        .split(':')
                        .next_back()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_default();

                    let current_block_number = match context.tip_block_number() {
                        Some(block_number) => *block_number,
                        None => resolver.last_block_number().await.context(
                            "Failed to query last block number while resolving $BLOCK_HASH",
                        )?,
                    };
                    let desired_block_number = current_block_number.saturating_sub(offset);

                    let block_hash = resolver
                        .block_hash(desired_block_number.into())
                        .await
                        .context("Failed to resolve block hash for desired block number")?;

                    Ok(U256::from_be_bytes(block_hash.0))
                } else if item == Self::BLOCK_NUMBER_VARIABLE {
                    let current_block_number = match context.tip_block_number() {
                        Some(block_number) => *block_number,
                        None => resolver.last_block_number().await.context(
                            "Failed to query last block number while resolving $BLOCK_NUMBER",
                        )?,
                    };
                    Ok(U256::from(current_block_number))
                } else if item == Self::BLOCK_TIMESTAMP_VARIABLE {
                    resolver
                        .block_timestamp(context.resolve_block_number(BlockNumberOrTag::Latest))
                        .await
                        .map(U256::from)
                } else if let Some(variable_name) = item.strip_prefix(Self::VARIABLE_PREFIX) {
                    context
                        .variable(variable_name)
                        .context("Variable lookup failed")
                        .copied()
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

#[cfg(test)]
mod tests {

    use alloy::{eips::BlockNumberOrTag, json_abi::JsonAbi};
    use alloy_primitives::{BlockHash, BlockNumber, BlockTimestamp, ChainId, TxHash, address};
    use alloy_sol_types::SolValue;
    use std::collections::HashMap;

    use super::*;
    use crate::metadata::ContractIdent;

    struct MockResolver;

    impl ResolverApi for MockResolver {
        async fn chain_id(&self) -> anyhow::Result<ChainId> {
            Ok(0x123)
        }

        async fn block_gas_limit(&self, _: BlockNumberOrTag) -> anyhow::Result<u128> {
            Ok(0x1234)
        }

        async fn block_coinbase(&self, _: BlockNumberOrTag) -> anyhow::Result<Address> {
            Ok(Address::ZERO)
        }

        async fn block_difficulty(&self, _: BlockNumberOrTag) -> anyhow::Result<U256> {
            Ok(U256::from(0x12345u128))
        }

        async fn block_base_fee(&self, _: BlockNumberOrTag) -> anyhow::Result<u64> {
            Ok(0x100)
        }

        async fn block_hash(&self, _: BlockNumberOrTag) -> anyhow::Result<BlockHash> {
            Ok([0xEE; 32].into())
        }

        async fn block_timestamp(&self, _: BlockNumberOrTag) -> anyhow::Result<BlockTimestamp> {
            Ok(0x123456)
        }

        async fn last_block_number(&self) -> anyhow::Result<BlockNumber> {
            Ok(0x1234567)
        }

        async fn transaction_gas_price(&self, _: &TxHash) -> anyhow::Result<u128> {
            Ok(0x200)
        }
    }

    #[tokio::test]
    async fn test_encoded_input_uint256() {
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

        let input = Input {
            instance: ContractInstance::new("Contract"),
            method: Method::FunctionName("store".to_owned()),
            calldata: Calldata::new_compound(["42"]),
            ..Default::default()
        };

        let mut contracts = HashMap::new();
        contracts.insert(
            ContractInstance::new("Contract"),
            (ContractIdent::new("Contract"), Address::ZERO, parsed_abi),
        );

        let resolver = MockResolver;
        let context = ResolutionContext::default().with_deployed_contracts(&contracts);
        let encoded = input.encoded_input(&resolver, context).await.unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (u64,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(decoded.0, 42);
    }

    #[tokio::test]
    async fn test_encoded_input_address_with_signature() {
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

        let input: Input = Input {
            instance: "Contract".to_owned().into(),
            method: Method::FunctionName("send(address)".to_owned()),
            calldata: Calldata::new_compound(["0x1000000000000000000000000000000000000001"]),
            ..Default::default()
        };

        let mut contracts = HashMap::new();
        contracts.insert(
            ContractInstance::new("Contract"),
            (ContractIdent::new("Contract"), Address::ZERO, parsed_abi),
        );

        let resolver = MockResolver;
        let context = ResolutionContext::default().with_deployed_contracts(&contracts);
        let encoded = input.encoded_input(&resolver, context).await.unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (alloy_primitives::Address,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }

    #[tokio::test]
    async fn test_encoded_input_address() {
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

        let input: Input = Input {
            instance: ContractInstance::new("Contract"),
            method: Method::FunctionName("send".to_owned()),
            calldata: Calldata::new_compound(["0x1000000000000000000000000000000000000001"]),
            ..Default::default()
        };

        let mut contracts = HashMap::new();
        contracts.insert(
            ContractInstance::new("Contract"),
            (ContractIdent::new("Contract"), Address::ZERO, parsed_abi),
        );

        let resolver = MockResolver;
        let context = ResolutionContext::default().with_deployed_contracts(&contracts);
        let encoded = input.encoded_input(&resolver, context).await.unwrap();
        assert!(encoded.0.starts_with(&selector));

        type T = (alloy_primitives::Address,);
        let decoded: T = T::abi_decode(&encoded.0[4..]).unwrap();
        assert_eq!(
            decoded.0,
            address!("0x1000000000000000000000000000000000000001")
        );
    }

    async fn resolve_calldata_item(
        input: &str,
        deployed_contracts: &HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>,
        resolver: &impl ResolverApi,
    ) -> anyhow::Result<U256> {
        let context = ResolutionContext::default().with_deployed_contracts(deployed_contracts);
        CalldataItem::new(input).resolve(resolver, context).await
    }

    #[tokio::test]
    async fn resolver_can_resolve_chain_id_variable() {
        // Arrange
        let input = "$CHAIN_ID";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(MockResolver.chain_id().await.unwrap()))
    }

    #[tokio::test]
    async fn resolver_can_resolve_gas_limit_variable() {
        // Arrange
        let input = "$GAS_LIMIT";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(
                MockResolver
                    .block_gas_limit(Default::default())
                    .await
                    .unwrap()
            )
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_coinbase_variable() {
        // Arrange
        let input = "$COINBASE";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from_be_slice(
                MockResolver
                    .block_coinbase(Default::default())
                    .await
                    .unwrap()
                    .as_ref()
            )
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_block_difficulty_variable() {
        // Arrange
        let input = "$DIFFICULTY";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            MockResolver
                .block_difficulty(Default::default())
                .await
                .unwrap()
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_block_base_fee_variable() {
        // Arrange
        let input = "$BASE_FEE";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            MockResolver
                .block_base_fee(Default::default())
                .await
                .map(U256::from)
                .unwrap()
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_block_hash_variable() {
        // Arrange
        let input = "$BLOCK_HASH";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from_be_bytes(MockResolver.block_hash(Default::default()).await.unwrap().0)
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_block_number_variable() {
        // Arrange
        let input = "$BLOCK_NUMBER";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(MockResolver.last_block_number().await.unwrap())
        )
    }

    #[tokio::test]
    async fn resolver_can_resolve_block_timestamp_variable() {
        // Arrange
        let input = "$BLOCK_TIMESTAMP";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(
                MockResolver
                    .block_timestamp(Default::default())
                    .await
                    .unwrap()
            )
        )
    }

    #[tokio::test]
    async fn simple_addition_can_be_resolved() {
        // Arrange
        let input = "2 4 +";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(6));
    }

    #[tokio::test]
    async fn simple_subtraction_can_be_resolved() {
        // Arrange
        let input = "4 2 -";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(2));
    }

    #[tokio::test]
    async fn simple_multiplication_can_be_resolved() {
        // Arrange
        let input = "4 2 *";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(8));
    }

    #[tokio::test]
    async fn simple_division_can_be_resolved() {
        // Arrange
        let input = "4 2 /";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(resolved, U256::from(2));
    }

    #[tokio::test]
    async fn arithmetic_errors_are_not_panics() {
        // Arrange
        let input = "4 0 /";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        assert!(resolved.is_err())
    }

    #[tokio::test]
    async fn arithmetic_with_resolution_works() {
        // Arrange
        let input = "$BLOCK_NUMBER 10 +";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

        // Assert
        let resolved = resolved.expect("Failed to resolve argument");
        assert_eq!(
            resolved,
            U256::from(MockResolver.last_block_number().await.unwrap() + 10)
        );
    }

    #[tokio::test]
    async fn incorrect_number_of_arguments_errors() {
        // Arrange
        let input = "$BLOCK_NUMBER 10 + +";

        // Act
        let resolved = resolve_calldata_item(input, &Default::default(), &MockResolver).await;

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
