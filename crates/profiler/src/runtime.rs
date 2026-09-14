use crate::internal_prelude::*;

pub(crate) const ENTER_OPCODE: &str = "retester_opcode_enter";
pub(crate) const EXIT_OPCODE: &str = "retester_opcode_exit";
pub(crate) const ENTER_CALL: &str = "retester_call_enter";
pub(crate) const EXIT_CALL: &str = "retester_call_exit";
pub(crate) const TRANSACTION_RESULT: &str = "retester_transaction_result";
pub(crate) const HOST_FUNCTION_COUNT: u32 = 5;

thread_local! {
    static TRANSACTION_OUTPUT: RefCell<Option<TransactionOutput>> = const { RefCell::new(None) };
}

type Executor = WasmExecutor<(
    sp_io::SubstrateHostFunctions,
    cumulus_primitives_proof_size_hostfunction::storage_proof_size::HostFunctions,
    ProfilingHostFunctions,
)>;

pub struct ProfilingRuntime {
    wasm: Vec<u8>,
    hash: Vec<u8>,
    executor: Executor,
}

impl ProfilingRuntime {
    pub fn new(wasm: impl Into<Vec<u8>>) -> Self {
        let wasm = wasm.into();
        Self {
            hash: sp_io::hashing::blake2_256(&wasm).to_vec(),
            wasm,
            executor: Executor::builder().build(),
        }
    }

    pub fn call(
        &self,
        externalities: &mut dyn Externalities,
        method: &str,
        input: &[u8],
        event_capacity: usize,
    ) -> Result<ProfiledCall> {
        let code = WrappedRuntimeCode(self.wasm.as_slice().into());
        let runtime_code = RuntimeCode {
            code_fetcher: &code,
            heap_pages: None,
            hash: self.hash.clone(),
        };
        self.executor
            .runtime_version(externalities, &runtime_code)
            .context("Failed to prepare runtime for profiling")?;
        TRANSACTION_OUTPUT.with_borrow_mut(|output| *output = None);
        Recorder::initialize(event_capacity);
        let output = self
            .executor
            .call(
                externalities,
                &runtime_code,
                method,
                input,
                CallContext::Onchain { import: false },
            )
            .0
            .context("Runtime execution failed");
        let events = Recorder::finish();
        let transaction_output = TRANSACTION_OUTPUT.with_borrow_mut(take);
        Ok(ProfiledCall {
            output: output?,
            events: events?,
            transaction_output,
        })
    }
}

#[derive(Debug)]
pub struct ProfiledCall {
    pub output: Vec<u8>,
    pub events: ProcessedEventsAndRawEvents,
    pub transaction_output: Option<TransactionOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionOutput {
    Returned(Vec<u8>),
    Reverted(Vec<u8>),
}

impl TransactionOutput {
    fn capture(context: &mut dyn FunctionContext, pointer: u32) -> Result<()> {
        // ContractResult's supported wasm32 layout places the result at byte 112,
        // followed by the Vec at 116 and ReturnFlags at 128.
        let result = context
            .read_memory(pointer.into(), 132)
            .map_err(anyhow::Error::msg)?;
        if result[112] != 15 {
            return Ok(());
        }
        let data_pointer = u32::from_le_bytes(result[120..124].try_into()?);
        let data_length = u32::from_le_bytes(result[124..128].try_into()?);
        let data = context
            .read_memory(data_pointer.into(), data_length)
            .map_err(anyhow::Error::msg)?;
        let output = if result[128] & 1 == 0 {
            Self::Returned(data)
        } else {
            Self::Reverted(data)
        };
        TRANSACTION_OUTPUT.with_borrow_mut(|stored| *stored = Some(output));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProfilingHostFunctions;

impl HostFunctions for ProfilingHostFunctions {
    fn host_functions() -> Vec<&'static dyn HostFunction> {
        vec![
            &ProfilingHook::OpCodeEnter,
            &ProfilingHook::OpCodeExit,
            &ProfilingHook::CallEnter,
            &ProfilingHook::CallExit,
            &ProfilingHook::TransactionResult,
        ]
    }

    fn register_static<T: HostFunctionRegistry>(
        registry: &mut T,
    ) -> std::result::Result<(), T::Error> {
        registry.register_static(
            TRANSACTION_RESULT,
            |caller: wasmtime::Caller<'_, T::State>, pointer: i32| -> Result<()> {
                T::with_function_context(caller, |context| {
                    TransactionOutput::capture(context, pointer as u32)
                })
            },
        )?;
        registry.register_static(
            ENTER_OPCODE,
            |op_code: i32, ref_time: i64, proof_size: i64| -> Result<()> {
                let instant = Instant::now();
                Recorder::record(ProfilingEvent::OpCodeEnter {
                    op_code: u8::try_from(op_code).context("Invalid EVM opcode")?,
                    weight_consumed: Weight::from_parts(ref_time as u64, proof_size as u64),
                    instant,
                });
                Ok(())
            },
        )?;
        registry.register_static(
            EXIT_OPCODE,
            |op_code: i32, ref_time: i64, proof_size: i64| -> Result<()> {
                let instant = Instant::now();
                Recorder::record(ProfilingEvent::OpCodeExit {
                    op_code: u8::try_from(op_code).context("Invalid EVM opcode")?,
                    weight_consumed: Weight::from_parts(ref_time as u64, proof_size as u64),
                    instant,
                });
                Ok(())
            },
        )?;
        registry.register_static(
            ENTER_CALL,
            |caller: wasmtime::Caller<'_, T::State>,
             op_code: i32,
             selector: i32,
             input_len: i32,
             address_pointer: i32|
             -> Result<()> {
                let mut code_address = H160::default();
                T::with_function_context(caller, |context| {
                    context.read_memory_into(
                        (address_pointer as u32).into(),
                        code_address.as_bytes_mut(),
                    )
                })
                .map_err(anyhow::Error::msg)?;
                Recorder::record(ProfilingEvent::CallEnter {
                    op_code: u8::try_from(op_code).context("Invalid EVM opcode")?,
                    selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
                    code_address,
                });
                Ok(())
            },
        )?;
        registry.register_static(
            EXIT_CALL,
            |op_code: i32, selector: i32, input_len: i32| -> Result<()> {
                Recorder::record(ProfilingEvent::CallExit {
                    op_code: u8::try_from(op_code).context("Invalid EVM opcode")?,
                    selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
                });
                Ok(())
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfilingHook {
    OpCodeEnter,
    OpCodeExit,
    CallEnter,
    CallExit,
    TransactionResult,
}

impl HostFunction for ProfilingHook {
    fn name(&self) -> &str {
        match self {
            Self::OpCodeEnter => ENTER_OPCODE,
            Self::OpCodeExit => EXIT_OPCODE,
            Self::CallEnter => ENTER_CALL,
            Self::CallExit => EXIT_CALL,
            Self::TransactionResult => TRANSACTION_RESULT,
        }
    }

    fn signature(&self) -> Signature {
        Signature::new_with_args(match self {
            Self::OpCodeEnter | Self::OpCodeExit => {
                vec![ValueType::I32, ValueType::I64, ValueType::I64]
            }
            Self::CallEnter => vec![ValueType::I32; 4],
            Self::CallExit => vec![ValueType::I32; 3],
            Self::TransactionResult => vec![ValueType::I32],
        })
    }

    fn execute(
        &self,
        context: &mut dyn FunctionContext,
        args: &mut dyn Iterator<Item = Value>,
    ) -> sp_wasm_interface::Result<Option<Value>> {
        if let Self::TransactionResult = self {
            let (Some(Value::I32(pointer)), None) = (args.next(), args.next()) else {
                return Err("Invalid transaction result arguments".into());
            };
            TransactionOutput::capture(context, pointer as u32)
                .map_err(|error| error.to_string())?;
            return Ok(None);
        }
        let event = match (self, args.next(), args.next(), args.next(), args.next()) {
            (
                Self::OpCodeEnter,
                Some(Value::I32(op_code)),
                Some(Value::I64(ref_time)),
                Some(Value::I64(proof_size)),
                None,
            ) => {
                let instant = Instant::now();
                ProfilingEvent::OpCodeEnter {
                    op_code: u8::try_from(op_code).map_err(|_| "Invalid EVM opcode")?,
                    weight_consumed: Weight::from_parts(ref_time as u64, proof_size as u64),
                    instant,
                }
            }
            (
                Self::OpCodeExit,
                Some(Value::I32(op_code)),
                Some(Value::I64(ref_time)),
                Some(Value::I64(proof_size)),
                None,
            ) => {
                let instant = Instant::now();
                ProfilingEvent::OpCodeExit {
                    op_code: u8::try_from(op_code).map_err(|_| "Invalid EVM opcode")?,
                    weight_consumed: Weight::from_parts(ref_time as u64, proof_size as u64),
                    instant,
                }
            }
            (
                Self::CallEnter,
                Some(Value::I32(op_code)),
                Some(Value::I32(selector)),
                Some(Value::I32(input_len)),
                Some(Value::I32(address_pointer)),
            ) if args.next().is_none() => {
                let mut code_address = H160::default();
                context.read_memory_into(
                    (address_pointer as u32).into(),
                    code_address.as_bytes_mut(),
                )?;
                ProfilingEvent::CallEnter {
                    op_code: u8::try_from(op_code).map_err(|_| "Invalid EVM opcode")?,
                    selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
                    code_address,
                }
            }
            (
                Self::CallExit,
                Some(Value::I32(op_code)),
                Some(Value::I32(selector)),
                Some(Value::I32(input_len)),
                None,
            ) => ProfilingEvent::CallExit {
                op_code: u8::try_from(op_code).map_err(|_| "Invalid EVM opcode")?,
                selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
            },
            _ => return Err("Invalid profiling hook arguments".into()),
        };
        Recorder::record(event);
        Ok(None)
    }
}
