use crate::internal_prelude::*;

thread_local! {
    static EVENTS: RefCell<Vec<ProfilingEvent>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorder;

impl Recorder {
    pub fn initialize(capacity: usize) {
        EVENTS.with_borrow_mut(|events| *events = Vec::with_capacity(capacity));
    }

    pub fn record(event: ProfilingEvent) {
        EVENTS.with_borrow_mut(|events| events.push(event));
    }

    pub fn finish() -> Result<ProcessedEventsAndRawEvents> {
        let raw_events = EVENTS.with_borrow_mut(take);
        let processed_events = process_events(&raw_events)?;
        Ok(ProcessedEventsAndRawEvents {
            processed_events,
            raw_events,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfilingEvent {
    OpCodeEnter {
        op_code: u8,
        weight_consumed: Weight,
        instant: Instant,
    },
    OpCodeExit {
        op_code: u8,
        weight_consumed: Weight,
        instant: Instant,
    },
    CallEnter {
        op_code: u8,
        selector: Option<[u8; 4]>,
        code_address: H160,
    },
    CallExit {
        op_code: u8,
        selector: Option<[u8; 4]>,
    },
}

#[derive(Debug)]
pub struct ProcessedEventsAndRawEvents {
    pub processed_events: Vec<OpCodeMeasurement>,
    pub raw_events: Vec<ProfilingEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpCodeMeasurement {
    pub op_code: u8,
    pub weight_consumed: Weight,
    pub instant: Instant,
    pub elapsed: Duration,
    pub call_depth: usize,
    pub selector: Option<[u8; 4]>,
}

struct PendingOpCode {
    measurement_index: usize,
    op_code: u8,
    weight_consumed: Weight,
    instant: Instant,
    call_depth: usize,
    selector: Option<[u8; 4]>,
}

struct CallFrame {
    op_code: u8,
    selector: Option<[u8; 4]>,
    open_op_codes: usize,
}

fn process_events(events: &[ProfilingEvent]) -> Result<Vec<OpCodeMeasurement>> {
    let mut measurements = Vec::new();
    let mut open_op_codes = Vec::<PendingOpCode>::new();
    let mut calls = Vec::<CallFrame>::new();

    for event in events {
        match *event {
            ProfilingEvent::OpCodeEnter {
                op_code,
                weight_consumed,
                instant,
            } => {
                open_op_codes.push(PendingOpCode {
                    measurement_index: measurements.len(),
                    op_code,
                    weight_consumed,
                    instant,
                    call_depth: calls.len(),
                    selector: calls.last().and_then(|call| call.selector),
                });
                measurements.push(None);
            }
            ProfilingEvent::OpCodeExit {
                op_code,
                weight_consumed,
                instant,
            } => {
                let entered = open_op_codes
                    .pop()
                    .context("Opcode exit without an entry")?;
                ensure!(
                    entered.op_code == op_code,
                    "Opcode entry and exit do not match"
                );
                ensure!(
                    entered.call_depth == calls.len(),
                    "Opcode exited in a different call frame"
                );
                measurements[entered.measurement_index] = Some(OpCodeMeasurement {
                    op_code,
                    weight_consumed: weight_consumed
                        .checked_sub(&entered.weight_consumed)
                        .context("Consumed weight decreased during opcode execution")?,
                    instant: entered.instant,
                    elapsed: instant
                        .checked_duration_since(entered.instant)
                        .context("Opcode exit precedes its entry")?,
                    call_depth: entered.call_depth,
                    selector: entered.selector,
                });
            }
            ProfilingEvent::CallEnter {
                op_code, selector, ..
            } => {
                calls.push(CallFrame {
                    op_code,
                    selector,
                    open_op_codes: open_op_codes.len(),
                });
            }
            ProfilingEvent::CallExit { op_code, selector } => {
                let entered = calls.pop().context("Call exit without an entry")?;
                ensure!(
                    entered.op_code == op_code && entered.selector == selector,
                    "Call entry and exit do not match"
                );
                ensure!(
                    open_op_codes.len() == entered.open_op_codes,
                    "Call exited with unfinished opcodes"
                );
            }
        }
    }

    ensure!(
        open_op_codes.is_empty(),
        "Execution ended with unfinished opcodes"
    );
    ensure!(calls.is_empty(), "Execution ended with unfinished calls");
    Ok(measurements.into_iter().flatten().collect())
}
