use parity_scale_codec::Encode;
use revive_dt_profiler::prelude::*;
use sp_core::H160;
use sp_io::TestExternalities;
use sp_weights::Weight;
use std::time::{Duration, Instant};
use wasm_encoder::Section;

fn fixture() -> Vec<u8> {
    let mut wasm = wat::parse_str(include_str!("fixtures/opcodes.wat")).unwrap();
    wasm_encoder::CustomSection {
        name: "runtime_version".into(),
        data: sc_executor::RuntimeVersion::default().encode().into(),
    }
    .append_to(&mut wasm);
    wasm
}

#[test]
fn preserves_results_and_records_nested_opcodes_partial_refunds_and_early_returns() {
    // Arrange
    let wasm = fixture();
    let baseline = ProfilingRuntime::new(wasm.clone());
    let profiled = ProfilingRuntime::new(instrument_wasm(wasm).unwrap());
    let mut state = TestExternalities::default();

    // Act
    let baseline = baseline.call(&mut state.ext(), "execute", &[], 1).unwrap();
    let profiled = profiled.call(&mut state.ext(), "execute", &[], 1).unwrap();
    let measurements = profiled.events.processed_events;

    // Assert
    assert_eq!(profiled.output, baseline.output);
    assert_eq!(&profiled.output[..8], &17_u64.to_le_bytes());
    assert!(baseline.events.raw_events.is_empty());
    assert!(baseline.events.processed_events.is_empty());
    assert_eq!(
        measurements
            .iter()
            .map(|event| (
                event.op_code,
                event.weight_consumed,
                event.call_depth,
                event.selector
            ))
            .collect::<Vec<_>>(),
        vec![
            (0xf1, Weight::from_parts(15, 0), 0, None),
            (
                0x01,
                Weight::from_parts(7, 0),
                1,
                Some([0x12, 0x34, 0x56, 0x78])
            ),
            (0x55, Weight::from_parts(2, 0), 0, None),
            (0x00, Weight::zero(), 0, None),
        ]
    );
    let raw = profiled.events.raw_events;
    assert_eq!(raw.len(), 10);
    assert_eq!(
        raw[1],
        ProfilingEvent::CallEnter {
            op_code: 0xf1,
            selector: Some([0x12, 0x34, 0x56, 0x78]),
            code_address: H160::repeat_byte(0x22),
        }
    );
    assert_eq!(
        raw[4],
        ProfilingEvent::CallExit {
            op_code: 0xf1,
            selector: Some([0x12, 0x34, 0x56, 0x78])
        }
    );
    let parent = &measurements[0];
    let child = &measurements[1];
    assert!(child.instant >= parent.instant);
    assert!(child.instant + child.elapsed <= parent.instant + parent.elapsed);
}

#[test]
fn rejects_decreasing_weight_and_recovers_after_a_runtime_trap() {
    // Arrange
    let runtime = ProfilingRuntime::new(instrument_wasm(fixture()).unwrap());
    let mut state = TestExternalities::default();

    // Act
    let trapped = runtime.call(&mut state.ext(), "trap", &[], 0).unwrap_err();
    let decreasing_weight = runtime
        .call(&mut state.ext(), "decreasing_weight", &[], 0)
        .unwrap_err();
    let recovered = runtime.call(&mut state.ext(), "execute", &[], 0).unwrap();
    let measurements = recovered.events.processed_events;

    // Assert
    assert!(
        format!("{trapped:#}").contains("unreachable"),
        "{trapped:#}"
    );
    assert_eq!(
        decreasing_weight.to_string(),
        "Consumed weight decreased during opcode execution"
    );
    assert_eq!(measurements.len(), 4);
    assert_eq!(measurements[0].weight_consumed, Weight::from_parts(15, 0));
    assert_eq!(&recovered.output[..8], &17_u64.to_le_bytes());
}

#[test]
fn records_constructor_and_precompile_frames_and_skips_calls_without_a_frame() {
    // Arrange
    let runtime = ProfilingRuntime::new(instrument_wasm(fixture()).unwrap());
    let mut state = TestExternalities::default();

    // Act
    let constructor = runtime.call(&mut state.ext(), "create", &[], 8).unwrap();
    let precompile = runtime
        .call(&mut state.ext(), "precompile", &[], 8)
        .unwrap();
    let no_frame = runtime.call(&mut state.ext(), "no_frame", &[], 8).unwrap();

    // Assert
    assert_eq!(constructor.events.raw_events.len(), 6);
    assert_eq!(
        constructor.events.raw_events[1],
        ProfilingEvent::CallEnter {
            op_code: 0xf0,
            selector: None,
            code_address: H160::repeat_byte(0x33),
        }
    );
    assert_eq!(
        constructor.events.raw_events[4],
        ProfilingEvent::CallExit {
            op_code: 0xf0,
            selector: None
        }
    );
    assert_eq!(constructor.events.processed_events[1].call_depth, 1);
    assert_eq!(precompile.events.raw_events.len(), 4);
    assert_eq!(
        precompile.events.raw_events[1],
        ProfilingEvent::CallEnter {
            op_code: 0xfa,
            selector: Some([0x12, 0x34, 0x56, 0x78]),
            code_address: H160::repeat_byte(0x22),
        }
    );
    assert_eq!(
        precompile.events.raw_events[2],
        ProfilingEvent::CallExit {
            op_code: 0xfa,
            selector: Some([0x12, 0x34, 0x56, 0x78])
        }
    );
    assert_eq!(precompile.events.processed_events.len(), 1);
    assert_eq!(no_frame.events.raw_events.len(), 2);
    assert_eq!(no_frame.events.processed_events[0].call_depth, 0);
}

#[test]
fn keeps_recordings_on_separate_threads_and_drains_them_at_finish() {
    // Arrange
    let instant = Instant::now();
    Recorder::initialize(0);
    Recorder::record(ProfilingEvent::OpCodeEnter {
        op_code: 1,
        weight_consumed: Weight::from_parts(100, 0),
        instant,
    });

    // Act
    let other = std::thread::spawn(move || {
        Recorder::initialize(0);
        Recorder::record(ProfilingEvent::OpCodeEnter {
            op_code: 2,
            weight_consumed: Weight::from_parts(200, 0),
            instant,
        });
        Recorder::record(ProfilingEvent::OpCodeExit {
            op_code: 2,
            weight_consumed: Weight::from_parts(205, 0),
            instant: instant + Duration::from_nanos(20),
        });
        Recorder::finish().unwrap()
    })
    .join()
    .unwrap();
    Recorder::record(ProfilingEvent::OpCodeExit {
        op_code: 1,
        weight_consumed: Weight::from_parts(103, 0),
        instant: instant + Duration::from_nanos(30),
    });
    let current = Recorder::finish().unwrap();
    let drained = Recorder::finish().unwrap();

    // Assert
    assert_eq!(other.raw_events.len(), 2);
    assert_eq!(current.raw_events.len(), 2);
    assert_eq!(
        other.processed_events[0].weight_consumed,
        Weight::from_parts(5, 0)
    );
    let measurements = current.processed_events;
    assert_eq!(measurements[0].op_code, 1);
    assert_eq!(measurements[0].weight_consumed, Weight::from_parts(3, 0));
    assert_eq!(measurements[0].elapsed, Duration::from_nanos(30));
    assert!(drained.raw_events.is_empty());
    assert!(drained.processed_events.is_empty());
}

#[test]
fn rejects_reinstrumentation_and_a_missing_dispatch_symbol() {
    // Arrange
    let wasm = instrument_wasm(fixture()).unwrap();
    let incompatible = wat::parse_str(include_str!("fixtures/opcodes.wat").replace(
        "pallet_revive::vm::evm::instructions::exec_instruction",
        "pallet_revive::vm::evm::instructions::different_dispatch",
    ))
    .unwrap();

    // Act
    let repeated = instrument_wasm(wasm).unwrap_err();
    let changed = instrument_wasm(incompatible).unwrap_err();

    // Assert
    assert_eq!(repeated.to_string(), "Wasm is already instrumented");
    assert_eq!(
        changed.to_string(),
        "Missing EVM dispatch or weight getter symbol"
    );
}

#[test]
fn captures_successful_and_reverted_transaction_outputs_without_carrying_them_between_calls() {
    // Arrange
    let runtime = ProfilingRuntime::new(instrument_wasm(fixture()).unwrap());
    let mut state = TestExternalities::default();

    // Act
    let success = runtime
        .call(&mut state.ext(), "transaction", &[], 1)
        .unwrap();
    let revert = runtime
        .call(&mut state.ext(), "reverted_transaction", &[], 1)
        .unwrap();
    let unrelated = runtime.call(&mut state.ext(), "execute", &[], 1).unwrap();

    // Assert
    assert_eq!(
        success.transaction_output,
        Some(TransactionOutput::Returned(vec![0x12, 0x34, 0x56, 0x78]))
    );
    assert_eq!(
        revert.transaction_output,
        Some(TransactionOutput::Reverted(vec![0x12, 0x34, 0x56, 0x78]))
    );
    assert_eq!(unrelated.transaction_output, None);
}
