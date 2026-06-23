use crate::internal_prelude::*;

#[derive(Debug)]
pub struct LighthouseNodeProcess {
    _process: NodeProcess,
    kurtosis_binary_path: PathBuf,
    enclave_name: OsString,
    http_url: String,
    ws_url: String,
}

impl LighthouseNodeProcess {
    const READY_MARKER: &str = "RUNNING";
    const ERROR_MARKER: &str = "Error encountered";

    pub fn new(
        kurtosis_binary_path: impl AsRef<Path>,
        enclave_name: impl AsRef<OsStr>,
        wrapper_directory: impl AsRef<Path>,
        args_file: impl AsRef<Path>,
        logs_directory: impl AsRef<Path>,
        start_timeout: Duration,
    ) -> Result<Self> {
        let process = NodeProcess::builder(kurtosis_binary_path.as_ref())
            .arg("run")
            .arg("--enclave")
            .arg(enclave_name.as_ref())
            .arg(wrapper_directory.as_ref())
            .arg("--args-file")
            .arg(args_file.as_ref())
            .log("kurtosis", logs_directory)
            .wait_for_startup(
                WaitForStartupSentinel::new(start_timeout, Self::READY_MARKER)
                    .with_failed_startup_if_encountered(Self::ERROR_MARKER),
            )
            .build()
            .context("Failed to start the kurtosis process")?;

        Self {
            _process: process,
            kurtosis_binary_path: kurtosis_binary_path.as_ref().to_path_buf(),
            enclave_name: enclave_name.as_ref().to_os_string(),
            // Filled in later to allow for node cleanups
            http_url: Default::default(),
            ws_url: Default::default(),
        }
        .fill_in_urls()
    }

    pub fn http_url(&self) -> &str {
        &self.http_url
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    fn fill_in_urls(mut self) -> Result<Self> {
        let enclave_inspection_result = Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("inspect")
            .arg(self.enclave_name.as_os_str())
            .run_and_get_output()
            .map(|output| output.stdout)?;

        self.http_url = enclave_inspection_result
            .split("el-1-geth-lighthouse")
            .nth(1)
            .and_then(|str| str.split(" rpc").nth(1))
            .and_then(|str| str.split("->").nth(1))
            .and_then(|str| str.split("\n").next())
            .and_then(|str| str.trim().split(" ").next())
            .map(|str| format!("http://{}", str.trim()))
            .context("Failed to find the HTTP connection string of Kurtosis")?;
        self.ws_url = enclave_inspection_result
            .split("el-1-geth-lighthouse")
            .nth(1)
            .and_then(|str| str.split("ws").nth(1))
            .and_then(|str| str.split("->").nth(1))
            .and_then(|str| str.split("\n").next())
            .and_then(|str| str.trim().split(" ").next())
            .map(|str| format!("ws://{}", str.trim()))
            .context("Failed to find the WS connection string of Kurtosis")?;

        Ok(self)
    }
}

impl Drop for LighthouseNodeProcess {
    fn drop(&mut self) {
        Command::new(self.kurtosis_binary_path.as_path())
            .arg("enclave")
            .arg("rm")
            .arg("-f")
            .arg(self.enclave_name.as_os_str())
            .run_and_get_output()
            .expect("Failed to shut down the lighthouse node");
    }
}
