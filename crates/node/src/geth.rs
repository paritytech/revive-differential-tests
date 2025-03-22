//! The go-ethereum node implementation.

use std::{
    fs::{File, create_dir, exists},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
};

use alloy::{
    genesis::Genesis,
    rpc::types::trace::geth::{DiffMode, PreStateFrame},
};
use revive_dt_config::get_args;
use revive_dt_node_interaction::{
    EthereumNode, trace::trace_transaction, transaction::execute_transaction,
};

use crate::Node;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct Instance {
    connection_string: String,
    directory: PathBuf,
    geth: PathBuf,
    id: u32,
}

impl Instance {
    pub fn new() -> anyhow::Result<Self> {
        let args = get_args();

        let geth_directory = args.working_directory.join("geth");
        if !exists(&geth_directory)? {
            create_dir(&geth_directory)?;
        }

        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let directory = geth_directory.join(id.to_string());

        let connection_string = directory.join("geth.ipc").display().to_string();

        Ok(Self {
            connection_string,
            directory,
            geth: args.geth.clone(),
            id,
        })
    }
}

impl Instance {
    /// Call `init` on the node to configure it's genesis.
    fn init(&mut self, genesis: Genesis) -> anyhow::Result<()> {
        let genesis_path = self.directory.join("genesis.json");

        let mut file = File::create(&genesis_path)?;
        serde_json::to_writer_pretty(&mut file, &genesis)?;

        if !Command::new(&self.geth)
            .arg("--datadir")
            .arg(self.directory.join("data"))
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?
            .wait()?
            .success()
        {
            anyhow::bail!("failed to initialize geth node {:?}", &self);
        }

        Ok(())
    }
}

impl EthereumNode for Instance {
    fn execute_transaction(
        &self,
        transaction_request: alloy::rpc::types::TransactionRequest,
    ) -> anyhow::Result<alloy::rpc::types::TransactionReceipt> {
        execute_transaction(transaction_request, self.connection_string())
    }

    fn trace_transaction(
        &self,
        transaction_receipt: alloy::rpc::types::TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        trace_transaction(
            transaction_receipt,
            Default::default(),
            self.connection_string(),
        )
    }
}

impl Node for Instance {
    fn connection_string(&self) -> String {
        self.connection_string.clone()
    }

    fn shutdown(self) -> anyhow::Result<()> {
        Ok(())
    }

    fn spawn(&mut self, genesis: Genesis) -> anyhow::Result<&mut Self> {
        self.init(genesis)?;

        Ok(self)
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
