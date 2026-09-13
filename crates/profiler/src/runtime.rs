use crate::internal_prelude::*;

pub(crate) const ENTER_OPCODE: &str = "retester_opcode_enter";
pub(crate) const EXIT_OPCODE: &str = "retester_opcode_exit";
pub(crate) const ENTER_CALL: &str = "retester_call_enter";
pub(crate) const EXIT_CALL: &str = "retester_call_exit";
pub(crate) const HOST_FUNCTION_COUNT: u32 = 4;

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
        Ok(ProfiledCall {
            output: output?,
            events: events?,
        })
    }
}

#[derive(Debug)]
pub struct ProfiledCall {
    pub output: Vec<u8>,
    pub events: ProcessedEventsAndRawEvents,
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
        ]
    }

    fn register_static<T: HostFunctionRegistry>(
        registry: &mut T,
    ) -> std::result::Result<(), T::Error> {
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
            |op_code: i32, selector: i32, input_len: i32| -> Result<()> {
                Recorder::record(ProfilingEvent::CallEnter {
                    op_code: u8::try_from(op_code).context("Invalid EVM opcode")?,
                    selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
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
}

impl HostFunction for ProfilingHook {
    fn name(&self) -> &str {
        match self {
            Self::OpCodeEnter => ENTER_OPCODE,
            Self::OpCodeExit => EXIT_OPCODE,
            Self::CallEnter => ENTER_CALL,
            Self::CallExit => EXIT_CALL,
        }
    }

    fn signature(&self) -> Signature {
        Signature::new_with_args(match self {
            Self::OpCodeEnter | Self::OpCodeExit => {
                vec![ValueType::I32, ValueType::I64, ValueType::I64]
            }
            Self::CallEnter | Self::CallExit => vec![ValueType::I32; 3],
        })
    }

    fn execute(
        &self,
        _: &mut dyn FunctionContext,
        args: &mut dyn Iterator<Item = Value>,
    ) -> sp_wasm_interface::Result<Option<Value>> {
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
                None,
            ) => ProfilingEvent::CallEnter {
                op_code: u8::try_from(op_code).map_err(|_| "Invalid EVM opcode")?,
                selector: (input_len as u32 >= 4).then(|| selector.to_le_bytes()),
            },
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
