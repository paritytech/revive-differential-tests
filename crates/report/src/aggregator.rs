//! Implementation of the report aggregator task which consumes the events sent by the various
//! reporters and combines them into a single unified report.

use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use revive_dt_config::Arguments;
use revive_dt_format::corpus::Corpus;
use serde::Serialize;
use tokio::sync::{
    broadcast::{Sender, channel},
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use crate::{
    SubscribeToEventsEvent,
    reporter_event::ReporterEvent,
    runner_event::{CorpusFileDiscoveryEvent, MetadataFileDiscoveryEvent, Reporter, RunnerEvent},
};

pub struct ReportAggregator {
    /* Internal Report State */
    report: Report,
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
                RunnerEvent::ExecutionCompleted(..) => break,
                RunnerEvent::SubscribeToEvents(event) => {
                    self.handle_subscribe_to_events_event(*event);
                }
                RunnerEvent::CorpusFileDiscovery(event) => {
                    self.handle_corpus_file_discovered_event(*event)
                }
                RunnerEvent::MetadataFileDiscovery(event) => {
                    self.handle_metadata_file_discovery_event(*event);
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
        self.report.metadata_files.insert(event.path);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The configuration that the tool was started up with.
    pub config: Arguments,
    /// The list of corpus files that the tool found.
    pub corpora: Vec<Corpus>,
    /// The list of metadata files that were found by the tool.
    pub metadata_files: BTreeSet<PathBuf>,
}

impl Report {
    pub fn new(config: Arguments) -> Self {
        Self {
            config,
            corpora: Default::default(),
            metadata_files: Default::default(),
        }
    }
}
