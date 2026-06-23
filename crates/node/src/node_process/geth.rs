use crate::internal_prelude::*;

#[derive(Debug)]
pub struct GethProcess {
    _process: NodeProcess,
    url: String,
}

impl GethProcess {
    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    pub fn new(
        binary_path: impl AsRef<Path>,
        genesis_path: impl AsRef<Path>,
        ipc_path: impl AsRef<Path>,
        data_directory: impl AsRef<Path>,
        logs_directory: impl AsRef<Path>,
        logging_level: impl AsRef<OsStr>,
        start_timeout: Duration,
    ) -> Result<Self> {
        Command::new(binary_path.as_ref())
            .arg("--state.scheme")
            .arg("hash")
            .arg("init")
            .arg("--datadir")
            .arg(data_directory.as_ref())
            .arg(genesis_path.as_ref())
            .run_and_get_output()
            .context("Failed to run geth init")?;

        let process = NodeProcess::builder(binary_path.as_ref())
            .arg("--dev")
            .arg("--datadir")
            .arg(data_directory.as_ref())
            .arg("--ipcpath")
            .arg(ipc_path.as_ref())
            .arg("--nodiscover")
            .arg("--maxpeers")
            .arg("0")
            .arg("--txlookuplimit")
            .arg("0")
            .arg("--cache.blocklogs")
            .arg("512")
            .arg("--state.scheme")
            .arg("hash")
            .arg("--syncmode")
            .arg("full")
            .arg("--gcmode")
            .arg("archive")
            .arg("--verbosity")
            .arg(logging_level)
            .arg("--rpc.batch-request-limit")
            .arg("0")
            .log("geth", logs_directory)
            .wait_for_startup(
                WaitForStartupSentinel::new(start_timeout, Self::READY_MARKER)
                    .with_failed_startup_if_encountered(Self::ERROR_MARKER),
            )
            .build()
            .context("Failed to start the geth process")?;

        let url = ipc_path.as_ref().display().to_string();

        Ok(Self {
            _process: process,
            url,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
