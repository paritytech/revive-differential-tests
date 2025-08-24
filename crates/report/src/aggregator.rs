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
use revive_dt_config::Arguments;
use revive_dt_format::{case::CaseIdx, corpus::Corpus};
use serde::Serialize;
use serde_with::{DisplayFromStr, serde_as};
use tokio::sync::{
    broadcast::{Sender, channel},
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use crate::{
    SubscribeToEventsEvent, TestCaseDiscoveryEvent, TestIgnoredEvent, TestSpecifier,
    common::MetadataFilePath,
    reporter_event::ReporterEvent,
    runner_event::{CorpusFileDiscoveryEvent, MetadataFileDiscoveryEvent, Reporter, RunnerEvent},
};

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
                RunnerEvent::TestIgnored(event) => {
                    self.handle_test_ignored_event(*event);
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

    fn handle_test_ignored_event(&mut self, event: TestIgnoredEvent) {
        // Remove this from the set of cases we're tracking
        self.remaining_cases
            .entry(event.test_specifier.metadata_file_path.clone().into())
            .or_default()
            .entry(event.test_specifier.solc_mode.clone())
            .or_default()
            .remove(&event.test_specifier.case_idx);

        // Add information on the fact that the case was ignored to the report.
        let test_case_report = self.test_case_report(&event.test_specifier);
        test_case_report.ignore = Some(TestCaseIgnoreInformation {
            is_ignored: true,
            reason: event.reason,
            additional_fields: event.additional_fields,
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
            config,
            corpora: Default::default(),
            metadata_files: Default::default(),
            test_case_information: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct TestCaseReport {
    /// Information related to the test case being ignored and why it's ignored.
    ignore: Option<TestCaseIgnoreInformation>,
}

/// Information related to the test case being ignored and why it's ignored.
#[derive(Clone, Debug, Serialize)]
pub struct TestCaseIgnoreInformation {
    /// A boolean that defines if the test case is ignored or not.
    pub is_ignored: bool,
    /// The reason behind the test case being ignored.
    pub reason: String,
    /// Additional fields that describe more information on why the test case is ignored.
    #[serde(flatten)]
    pub additional_fields: IndexMap<String, serde_json::Value>,
}
