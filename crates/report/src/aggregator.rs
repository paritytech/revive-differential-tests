//! Implementation of the report aggregator task which consumes the events sent by the various
//! reporters and combines them into a single unified report.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::OpenOptions,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use indexmap::IndexMap;
use revive_dt_compiler::Mode;
use revive_dt_config::{Arguments, TestingPlatform};
use revive_dt_format::{case::CaseIdx, corpus::Corpus};
use serde::Serialize;
use serde_with::{DisplayFromStr, serde_as};
use tokio::sync::{
    broadcast::{Sender, channel},
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use crate::*;

pub struct ReportAggregator {
    /* Internal Report State */
    report: Report,
    remaining_cases: HashMap<MetadataFilePath, HashMap<Mode, HashSet<CaseIdx>>>,
    /* Channels */
    runner_tx: Option<UnboundedSender<RunnerEvent>>,
    runner_rx: UnboundedReceiver<RunnerEvent>,
    listener_tx: Sender<ReporterEvent>,
}

impl ReportAggregator {
    pub fn new(config: Arguments) -> Self {
        let (runner_tx, runner_rx) = unbounded_channel::<RunnerEvent>();
        let (listener_tx, _) = channel::<ReporterEvent>(1024);
        Self {
            report: Report::new(config),
            remaining_cases: Default::default(),
            runner_tx: Some(runner_tx),
            runner_rx,
            listener_tx,
        }
    }

    pub fn into_task(mut self) -> (Reporter, impl Future<Output = Result<()>>) {
        let reporter = self
            .runner_tx
            .take()
            .map(Into::into)
            .expect("Can't fail since this can only be called once");
        (reporter, async move { self.aggregate().await })
    }

    async fn aggregate(mut self) -> Result<()> {
        while let Some(event) = self.runner_rx.recv().await {
            match event {
                RunnerEvent::SubscribeToEvents(event) => {
                    self.handle_subscribe_to_events_event(*event);
                }
                RunnerEvent::CorpusFileDiscovery(event) => {
                    self.handle_corpus_file_discovered_event(*event)
                }
                RunnerEvent::MetadataFileDiscovery(event) => {
                    self.handle_metadata_file_discovery_event(*event);
                }
                RunnerEvent::TestCaseDiscovery(event) => {
                    self.handle_test_case_discovery(*event);
                }
                RunnerEvent::TestSucceeded(event) => {
                    self.handle_test_succeeded_event(*event);
                }
                RunnerEvent::TestFailed(event) => {
                    self.handle_test_failed_event(*event);
                }
                RunnerEvent::TestIgnored(event) => {
                    self.handle_test_ignored_event(*event);
                }
                RunnerEvent::LeaderNodeAssigned(event) => {
                    self.handle_leader_node_assigned_event(*event);
                }
                RunnerEvent::FollowerNodeAssigned(event) => {
                    self.handle_follower_node_assigned_event(*event);
                }
            }
        }

        let file_name = {
            let current_timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let mut file_name = current_timestamp.to_string();
            file_name.push_str(".json");
            file_name
        };
        let file_path = self.report.config.directory().join(file_name);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(false)
            .open(file_path)?;
        serde_json::to_writer_pretty(file, &self.report)?;

        Ok(())
    }

    fn handle_subscribe_to_events_event(&self, event: SubscribeToEventsEvent) {
        let _ = event.tx.send(self.listener_tx.subscribe());
    }

    fn handle_corpus_file_discovered_event(&mut self, event: CorpusFileDiscoveryEvent) {
        self.report.corpora.push(event.corpus);
    }

    fn handle_metadata_file_discovery_event(&mut self, event: MetadataFileDiscoveryEvent) {
        self.report.metadata_files.insert(event.path.clone());
    }

    fn handle_test_case_discovery(&mut self, event: TestCaseDiscoveryEvent) {
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .insert(event.test_specifier.case_idx);
    }

    fn handle_test_succeeded_event(&mut self, event: TestSucceededEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .remove(&event.test_specifier.case_idx);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Succeeded {
            steps_executed: event.steps_executed,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_test_failed_event(&mut self, event: TestFailedEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .remove(&event.test_specifier.case_idx);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Failed {
            reason: event.reason,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_test_ignored_event(&mut self, event: TestIgnoredEvent) {
        // Remove this from the set of cases we're tracking since it has completed.
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .remove(&event.test_specifier.case_idx);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.status = Some(TestCaseStatus::Ignored {
            reason: event.reason,
            additional_fields: event.additional_fields,
        });
        self.handle_post_test_case_status_update(&event.test_specifier);
    }

    fn handle_post_test_case_status_update(&mut self, specifier: &TestSpecifier) {
        let remaining_cases = self
            .remaining_cases
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(specifier.solc_mode.clone())
            .or_default();
        if !remaining_cases.is_empty() {
            return;
        }

        let case_status = self
            .report
            .test_case_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(specifier.solc_mode.clone())
            .or_default()
            .iter()
            .map(|(case_idx, case_report)| {
                (
                    *case_idx,
                    case_report.status.clone().expect("Can't be uninitialized"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let event = ReporterEvent::MetadataFileSolcModeCombinationExecutionCompleted {
            metadata_file_path: specifier.metadata_file_path.clone().into(),
            mode: specifier.solc_mode.clone(),
            case_status,
        };

        // According to the documentation on send, the sending fails if there are no more receiver
        // handles. Therefore, this isn't an error that we want to bubble up or anything. If we fail
        // to send then we ignore the error.
        let _ = self.listener_tx.send(event);
    }

    fn handle_leader_node_assigned_event(&mut self, event: LeaderNodeAssignedEvent) {
        self.test_case_report(&event.test_specifier).leader_node = Some(TestCaseNodeInformation {
            id: event.id,
            platform: event.platform,
            connection_string: event.connection_string,
        });
    }

    fn handle_follower_node_assigned_event(&mut self, event: FollowerNodeAssignedEvent) {
        self.test_case_report(&event.test_specifier).follower_node =
            Some(TestCaseNodeInformation {
                id: event.id,
                platform: event.platform,
                connection_string: event.connection_string,
            });
    }

    fn test_case_report(&mut self, specifier: &TestSpecifier) -> &mut TestCaseReport {
        self.report
            .test_case_information
            .entry(specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(specifier.solc_mode.clone())
            .or_default()
            .entry(specifier.case_idx)
            .or_default()
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The configuration that the tool was started up with.
    pub config: Arguments,
    /// The platform of the leader chain.
    pub leader_platform: TestingPlatform,
    /// The platform of the follower chain.
    pub follower_platform: TestingPlatform,
    /// The list of corpus files that the tool found.
    pub corpora: Vec<Corpus>,
    /// The list of metadata files that were found by the tool.
    pub metadata_files: BTreeSet<MetadataFilePath>,
    /// Information relating to each test case.
    #[serde_as(as = "BTreeMap<_, HashMap<DisplayFromStr, BTreeMap<DisplayFromStr, _>>>")]
    pub test_case_information:
        BTreeMap<MetadataFilePath, HashMap<Mode, BTreeMap<CaseIdx, TestCaseReport>>>,
}

impl Report {
    pub fn new(config: Arguments) -> Self {
        Self {
            leader_platform: config.leader,
            follower_platform: config.follower,
            config,
            corpora: Default::default(),
            metadata_files: Default::default(),
            test_case_information: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct TestCaseReport {
    /// Information on the status of the test case and whether it succeeded, failed, or was ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TestCaseStatus>,
    /// Information related to the leader node assigned to this test case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader_node: Option<TestCaseNodeInformation>,
    /// Information related to the follower node assigned to this test case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follower_node: Option<TestCaseNodeInformation>,
}

/// Information related to the status of the test. Could be that the test succeeded, failed, or that
/// it was ignored.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status")]
pub enum TestCaseStatus {
    /// The test case succeeded.
    Succeeded {
        /// The number of steps of the case that were executed.
        steps_executed: usize,
    },
    /// The test case failed.
    Failed {
        /// The reason for the failure of the test case.
        reason: String,
    },
    /// The test case was ignored. This variant carries information related to why it was ignored.
    Ignored {
        /// The reason behind the test case being ignored.
        reason: String,
        /// Additional fields that describe more information on why the test case is ignored.
        #[serde(flatten)]
        additional_fields: IndexMap<String, serde_json::Value>,
    },
}

/// Information related to the leader or follower node that's being used to execute the step.
#[derive(Clone, Debug, Serialize)]
pub struct TestCaseNodeInformation {
    /// The ID of the node that this case is being executed on.
    pub id: usize,
    /// The platform of the node.
    pub platform: TestingPlatform,
    /// The connection string of the node.
    pub connection_string: String,
}
