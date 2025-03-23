//! The go-ethereum node implementation.

use std::{
    fs::{File, create_dir, exists, remove_dir_all},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use alloy::rpc::types::{
    TransactionReceipt, TransactionRequest,
    trace::geth::{DiffMode, PreStateFrame},
};
use revive_dt_config::Arguments;
use revive_dt_node_interaction::{
    EthereumNode, trace::trace_transaction, transaction::execute_transaction,
};

use crate::Node;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// The go-ethereum node instance implementation.
///
/// Implements helpers to initialize, spawn and wait the node.
///
/// Assumes dev mode and IPC only (`P2P`, `http`` etc. are kept disabled).
///
/// Prunes the child process and the base directory on drop.
#[derive(Debug)]
pub struct Instance {
    connection_string: String,
    base_directory: PathBuf,
    data_directory: PathBuf,
    geth: PathBuf,
    id: u32,
    handle: Option<Child>,
    network_id: u64,
    start_timeout: u64,
}

impl Instance {
    const BASE_DIRECTORY: &str = "geth";
    const DATA_DIRECTORY: &str = "data";

    const IPC_FILE: &str = "geth.ipc";
    const GENESIS_JSON_FILE: &str = "genesis.json";

    const READY_MARKER: &str = "IPC endpoint opened";
    const ERROR_MARKER: &str = "Fatal:";

    /// Create a new uninitialized instance.
    pub fn new(config: &Arguments) -> anyhow::Result<Self> {
        let geth_directory = config
            .working_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("config did not provide working directory"))?
            .join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = geth_directory.join(id.to_string());

        Ok(Self {
            connection_string: base_directory.join(Self::IPC_FILE).display().to_string(),
            data_directory: base_directory.join(Self::DATA_DIRECTORY),
            base_directory,
            geth: config.geth.clone(),
            id,
            handle: None,
            network_id: config.network_id,
            start_timeout: config.geth_start_timeout,
        })
    }

    /// Create the node directory and call `geth init` to configure the genesis.
    fn init(&mut self, genesis: String) -> anyhow::Result<&mut Self> {
        let geth_directory = self.base_directory.parent().expect("the id should be set");
        if !exists(geth_directory)? {
            create_dir(geth_directory)?;
        }
        create_dir(&self.base_directory)?;

        let genesis_path = self.base_directory.join(Self::GENESIS_JSON_FILE);
        File::create(&genesis_path)?.write_all(genesis.as_bytes())?;

        let mut child = Command::new(&self.geth)
            .arg("init")
            .arg("--datadir")
            .arg(&self.data_directory)
            .arg(genesis_path)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;

        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("should be piped")
            .read_to_string(&mut stderr)?;

        if !child.wait()?.success() {
            anyhow::bail!("failed to initialize geth node #{:?}: {stderr}", &self.id);
        }

        Ok(self)
    }

    /// Spawn the go-ethereum node child process.
    ///
    /// [Instance::init] must be called priorly.
    fn spawn_process(&mut self) -> anyhow::Result<&mut Self> {
        self.handle = Command::new(&self.geth)
            .arg("--dev")
            .arg("--datadir")
            .arg(&self.data_directory)
            .arg("--ipcpath")
            .arg(&self.connection_string)
            .arg("--networkid")
            .arg(self.network_id.to_string())
            .arg("--nodiscover")
            .arg("--maxpeers")
            .arg("0")
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?
            .into();
        Ok(self)
    }

    /// Wait for the g-ethereum node child process getting ready.
    ///
    /// [Instance::spawn_process] must be called priorly.
    fn wait_ready(&mut self) -> anyhow::Result<&mut Self> {
        // Thanks clippy but geth is a server; we don't `wait` but eventually kill it.
        #[allow(clippy::zombie_processes)]
        let mut child = self.handle.take().expect("should be spawned");
        let start_time = Instant::now();
        let maximum_wait_time = Duration::from_millis(self.start_timeout);
        let mut stderr = BufReader::new(child.stderr.take().expect("should be piped")).lines();
        let error = loop {
            let Some(Ok(line)) = stderr.next() else {
                break "child process stderr reading error".to_string();
            };
            if line.contains(Self::ERROR_MARKER) {
                break line;
            }
            if line.contains(Self::READY_MARKER) {
                // Keep stderr alive
                // https://github.com/alloy-rs/alloy/issues/2091#issuecomment-2676134147
                thread::spawn(move || for _ in stderr.by_ref() {});

                self.handle = child.into();
                return Ok(self);
            }
            if Instant::now().duration_since(start_time) > maximum_wait_time {
                break "spawn timeout".to_string();
            }
        };

        let _ = child.kill();
        anyhow::bail!("geth node #{} spawn error: {error}", self.id)
    }
}

impl EthereumNode for Instance {
    fn execute_transaction(
        &self,
        transaction: TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        execute_transaction(transaction, self.connection_string())
    }

    fn trace_transaction(
        &self,
        transaction: TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        trace_transaction(transaction, Default::default(), self.connection_string())
    }
}

impl Node for Instance {
    fn connection_string(&self) -> String {
        self.connection_string.clone()
    }

    fn shutdown(self) -> anyhow::Result<()> {
        Ok(())
    }

    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.init(genesis)?.spawn_process()?.wait_ready()?;
        Ok(())
    }

    fn state_diff(
        &self,
        transaction: alloy::rpc::types::TransactionReceipt,
    ) -> anyhow::Result<DiffMode> {
        match self
            .trace_transaction(transaction)?
            .try_into_pre_state_frame()?
        {
            PreStateFrame::Diff(diff) => Ok(diff),
            _ => anyhow::bail!("expected a diff mode trace"),
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(child) = self.handle.as_mut() {
            let _ = child.kill();
        }
        if self.base_directory.exists() {
            let _ = remove_dir_all(&self.base_directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use revive_dt_config::Arguments;
    use temp_dir::TempDir;

    use crate::{GENESIS_JSON, Node};

    use super::Instance;

    fn test_config() -> (Arguments, TempDir) {
        let mut config = Arguments::default();
        let temp_dir = TempDir::new().unwrap();
        config.working_directory = temp_dir.path().to_path_buf().into();

        (config, temp_dir)
    }

    #[test]
    fn init_works() {
        Instance::new(&test_config().0)
            .unwrap()
            .init(GENESIS_JSON.to_string())
            .unwrap();
    }

    #[test]
    fn spawn_works() {
        Instance::new(&test_config().0)
            .unwrap()
            .spawn(GENESIS_JSON.to_string())
            .unwrap();
    }
}
