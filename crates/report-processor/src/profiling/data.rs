use crate::internal_prelude::*;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ProfilingData {
    pub runtime_branch: String,
    pub runtime_commit: String,
    pub workloads: Vec<Workload>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Workload {
    pub metadata_file_path: String,
    pub case_index: usize,
    pub mode: Mode,
    pub name: Option<String>,
    pub function_names: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub deployments: BTreeMap<String, String>,
    #[serde(deserialize_with = "read_transactions")]
    pub transactions: Vec<RepeatedTransaction>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RepeatedTransaction {
    pub repeat_path: String,
    pub entry_point: Option<String>,
    pub op_codes: Vec<u8>,
    pub weights: Vec<Weight>,
    pub depths: Vec<usize>,
    pub selectors: Vec<Option<[u8; 4]>>,
    pub calls: Vec<CallFrame>,
    pub samples: Vec<Sample>,
}

impl RepeatedTransaction {
    fn new(repeat_path: String, transaction: &Transaction) -> Self {
        let events = &transaction.processed_events;
        Self {
            repeat_path,
            entry_point: transaction.entry_point.clone(),
            op_codes: events.iter().map(|event| event.op_code).collect(),
            weights: events.iter().map(|event| event.weight_consumed).collect(),
            depths: events.iter().map(|event| event.call_depth).collect(),
            selectors: events.iter().map(|event| event.selector).collect(),
            calls: transaction.raw_events.clone(),
            samples: Vec::new(),
        }
    }

    fn push(&mut self, transaction: Transaction) -> Result<()> {
        ensure!(
            transaction.processed_events.len() == self.op_codes.len()
                && transaction.raw_events == self.calls,
            "Repeat step {} changed its opcode sequence between repetitions",
            self.repeat_path
        );
        let mut started_at_ns = Vec::with_capacity(self.op_codes.len());
        let mut elapsed_ns = Vec::with_capacity(self.op_codes.len());
        for (index, event) in transaction.processed_events.into_iter().enumerate() {
            ensure!(
                event.op_code == self.op_codes[index]
                    && event.weight_consumed == self.weights[index]
                    && event.call_depth == self.depths[index]
                    && event.selector == self.selectors[index],
                "Repeat step {} changed opcode, weight or call frame at event {}",
                self.repeat_path,
                index
            );
            started_at_ns.push(u64::try_from(event.started_at.as_nanos())?);
            elapsed_ns.push(u64::try_from(event.elapsed.as_nanos())?);
        }
        self.samples.push(Sample {
            started_at_ns,
            elapsed_ns,
        });
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Sample {
    pub started_at_ns: Vec<u64>,
    pub elapsed_ns: Vec<u64>,
}

#[derive(Debug, Deserialize)]
struct Transaction {
    repeat_path: Option<String>,
    entry_point: Option<String>,
    processed_events: Vec<Measurement>,
    #[serde(deserialize_with = "read_call_frames")]
    raw_events: Vec<CallFrame>,
}

#[derive(Debug, Deserialize)]
struct Measurement {
    op_code: u8,
    weight_consumed: Weight,
    call_depth: usize,
    selector: Option<[u8; 4]>,
    started_at: Duration,
    elapsed: Duration,
}

// Read one transaction at a time. Repeated opcode metadata is stored once;
// every timing sample is kept for calculations in JavaScript.
fn read_transactions<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<RepeatedTransaction>, D::Error> {
    struct Transactions;

    impl<'de> Visitor<'de> for Transactions {
        type Value = Vec<RepeatedTransaction>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("profiling transactions")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut groups = Vec::<RepeatedTransaction>::new();
            while let Some(transaction) = sequence.next_element::<Transaction>()? {
                let Some(path) = &transaction.repeat_path else {
                    continue;
                };
                if transaction.processed_events.is_empty() {
                    continue;
                }
                let group = match groups.iter().position(|group| {
                    group.repeat_path == *path && group.entry_point == transaction.entry_point
                }) {
                    Some(index) => &mut groups[index],
                    None => {
                        let index = groups.len();
                        groups.push(RepeatedTransaction::new(path.clone(), &transaction));
                        &mut groups[index]
                    }
                };
                group.push(transaction).map_err(A::Error::custom)?;
            }
            Ok(groups)
        }
    }

    deserializer.deserialize_seq(Transactions)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CallFrame {
    pub parent_opcode: usize,
    pub first_opcode: usize,
    pub end_opcode: usize,
    pub selector: Option<[u8; 4]>,
    pub code_address: Option<String>,
}

fn read_call_frames<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<CallFrame>, D::Error> {
    #[derive(Deserialize)]
    #[serde(tag = "event")]
    enum Event {
        OpCodeEnter,
        OpCodeExit,
        CallEnter {
            selector: Option<[u8; 4]>,
            code_address: Option<String>,
        },
        CallExit,
    }

    struct Calls;

    impl<'de> Visitor<'de> for Calls {
        type Value = Vec<CallFrame>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("raw profiling events")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut calls = Vec::<CallFrame>::new();
            let mut open_calls = Vec::<usize>::new();
            let mut open_opcodes = Vec::new();
            let mut next_opcode = 0;
            while let Some(event) = sequence.next_element::<Event>()? {
                match event {
                    Event::OpCodeEnter => {
                        open_opcodes.push(next_opcode);
                        next_opcode += 1;
                    }
                    Event::OpCodeExit => {
                        open_opcodes
                            .pop()
                            .ok_or_else(|| A::Error::custom("Unmatched opcode exit"))?;
                    }
                    Event::CallEnter {
                        selector,
                        code_address,
                    } => {
                        let parent_opcode = *open_opcodes
                            .last()
                            .ok_or_else(|| A::Error::custom("Call without an opcode"))?;
                        open_calls.push(calls.len());
                        calls.push(CallFrame {
                            parent_opcode,
                            first_opcode: next_opcode,
                            end_opcode: next_opcode,
                            selector,
                            code_address,
                        });
                    }
                    Event::CallExit => {
                        let index = open_calls
                            .pop()
                            .ok_or_else(|| A::Error::custom("Unmatched call exit"))?;
                        calls[index].end_opcode = next_opcode;
                    }
                }
            }
            if !open_calls.is_empty() || !open_opcodes.is_empty() {
                return Err(A::Error::custom("Unfinished profiling intervals"));
            }
            Ok(calls)
        }
    }

    deserializer.deserialize_seq(Calls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn excludes_setup_and_keeps_each_repeated_step_and_timing_sample() {
        // Arrange
        let transaction = json!({
            "repeat_path": "2.1", "entry_point": "Verifier.verify(bytes)",
            "processed_events": [{"op_code": 57, "weight_consumed": {"ref_time": 9000, "proof_size": 0},
                "call_depth": 0, "selector": null,
                "started_at": {"secs": 0, "nanos": 10}, "elapsed": {"secs": 0, "nanos": 20}}],
            "raw_events": [{"event": "OpCodeEnter"}, {"event": "OpCodeExit"}]
        });
        let mut setup = transaction.clone();
        setup["repeat_path"] = json!(null);
        let mut second = transaction.clone();
        second["processed_events"][0]["elapsed"]["nanos"] = json!(40);
        let mut nested = transaction.clone();
        nested["repeat_path"] = json!("2.2.0");
        let workload = json!({"metadata_file_path": "test.json", "case_index": 0,
            "mode": "Y Mz S+".parse::<Mode>().unwrap(), "name": "Verifier", "function_names": {},
            "transactions": [setup, transaction, second, nested]});

        // Act
        let result = serde_json::from_value::<Workload>(workload).unwrap();

        // Assert
        assert_eq!(result.transactions.len(), 2);
        assert_eq!(result.transactions[0].repeat_path, "2.1");
        assert_eq!(
            result.transactions[0].weights,
            vec![Weight::from_parts(9000, 0)]
        );
        assert_eq!(result.transactions[0].samples[0].elapsed_ns, vec![20]);
        assert_eq!(result.transactions[0].samples[1].elapsed_ns, vec![40]);
        assert_eq!(result.transactions[1].repeat_path, "2.2.0");
        assert_eq!(result.transactions[1].samples.len(), 1);
    }

    #[test]
    fn retains_nested_and_empty_call_boundaries_from_raw_events() {
        // Arrange
        let transaction = json!({"repeat_path": "1.0", "entry_point": "Verifier.verify()",
        "processed_events": [], "raw_events": [
            {"event": "OpCodeEnter"},
            {"event": "CallEnter", "selector": [1, 2, 3, 4], "code_address": "0x0000000000000000000000000000000000000012"},
            {"event": "OpCodeEnter"}, {"event": "OpCodeExit"},
            {"event": "CallExit"}, {"event": "OpCodeExit"},
            {"event": "OpCodeEnter"},
            {"event": "CallEnter", "selector": null},
            {"event": "CallExit"}, {"event": "OpCodeExit"}
        ]});

        // Act
        let result = serde_json::from_value::<Transaction>(transaction).unwrap();

        // Assert
        assert_eq!(
            result.raw_events,
            vec![
                CallFrame {
                    parent_opcode: 0,
                    first_opcode: 1,
                    end_opcode: 2,
                    selector: Some([1, 2, 3, 4]),
                    code_address: Some("0x0000000000000000000000000000000000000012".to_owned()),
                },
                CallFrame {
                    parent_opcode: 2,
                    first_opcode: 3,
                    end_opcode: 3,
                    selector: None,
                    code_address: None,
                },
            ]
        );
    }
}
