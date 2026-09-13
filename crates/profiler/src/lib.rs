mod instrument;
mod recording;
mod runtime;

pub mod prelude {
    pub use crate::{
        instrument::instrument_wasm,
        recording::{OpCodeMeasurement, ProcessedEventsAndRawEvents, ProfilingEvent, Recorder},
        runtime::{ProfiledCall, ProfilingRuntime},
    };
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub(crate) use crate::{
        instrument::metadata::Metadata,
        runtime::{ENTER_CALL, ENTER_OPCODE, EXIT_CALL, EXIT_OPCODE, HOST_FUNCTION_COUNT},
    };

    pub use std::{
        cell::RefCell,
        collections::BTreeMap,
        convert::Infallible,
        mem::take,
        time::{Duration, Instant},
    };

    pub use anyhow::{Context as _, Result, bail, ensure};
    pub use rustc_demangle::demangle;
    pub use sc_executor::{RuntimeVersionOf, WasmExecutor};
    pub use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
    pub use sp_externalities::Externalities;
    pub use sp_wasm_interface::{
        Function as HostFunction, FunctionContext, HostFunctionRegistry, HostFunctions, Signature,
        Value, ValueType,
    };
    pub use sp_weights::Weight;
    pub use wasm_encoder::{
        self as encoder,
        reencode::{self, Reencode},
    };
    pub use wasm_encoder::{
        Instruction::{
            Call, End, GlobalGet, GlobalSet, I32Add, I32Const, I32GeU, I32Load, I32Sub, I64Load,
            If, LocalGet, LocalSet, LocalTee,
        },
        MemArg,
    };
    pub use wasmparser::{self as parser, Parser, Payload, Validator};
}
