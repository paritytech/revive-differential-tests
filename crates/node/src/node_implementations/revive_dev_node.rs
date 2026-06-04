use crate::internal_prelude::*;

#[derive(Debug)]

pub struct ReviveDevNode {
    id: usize,
    revive_dev_node_process: ReviveDevNodeProcess,
    eth_rpc_process: EthRpcProcess,
    _directories: NodeDirectories,
}

impl ReviveDevNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasReviveDevNodeConfiguration
        + Clone,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let wallet_config = context.as_wallet_configuration();
        let node_config = context.as_revive_dev_node_configuration();
        let rpc_config = context.as_eth_rpc_configuration();

        let id = NodeId::for_node("revive-dev-node");
        let directories = NodeDirectories::new(
            workdir_config.working_directory.as_path(),
            "revive-dev-node",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let chainspec_path = directories.base_directory().join("chainspec.json");

        let wallet = wallet_config.wallet();
        Self::init_chainspec(
            node_config.path.as_path(),
            &wallet,
            chainspec_path.as_path(),
        )
        .context("Failed to initialize the chainspec file")?;

        let revive_dev_node_process = ReviveDevNodeProcess::new(
            node_config.path.as_path(),
            chainspec_path,
            node_config.consensus.as_str(),
            directories.data_directory(),
            directories.logs_directory(),
            node_config.logging_level.as_str(),
            node_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn revive-dev-node"))?;

        let eth_rpc_process = EthRpcProcess::new(
            rpc_config.path.as_path(),
            directories.logs_directory(),
            revive_dev_node_process.url(),
            rpc_config.logging_level.as_str(),
            rpc_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?;

        Ok(Self {
            id: id.0,
            revive_dev_node_process,
            eth_rpc_process,
            _directories: directories,
        })
    }

    fn init_chainspec(
        binary_path: impl AsRef<Path>,
        wallet: &EthereumWallet,
        chainspec_path: impl AsRef<Path>,
    ) -> Result<()> {
        let chainspec =
            Self::chainspec(binary_path, wallet).context("Failed to create the chainspec")?;
        File::create(chainspec_path)
            .context("Failed to create the chainspec file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &chainspec)
                    .context("Failed to serialize chainspec to writer")
            })?;

        Ok(())
    }

    pub fn chainspec(binary_path: impl AsRef<Path>, wallet: &EthereumWallet) -> Result<Value> {
        static CACHED_BASE_CHAINSPECS: LazyLock<Arc<StdMutex<HashMap<PathBuf, Value>>>> =
            LazyLock::new(Default::default);

        let mut chainspec = match CACHED_BASE_CHAINSPECS
            .lock()
            .expect("poisoned")
            .entry(binary_path.as_ref().to_path_buf())
        {
            HashMapEntry::Occupied(entry) => entry.get().clone(),
            HashMapEntry::Vacant(entry) => {
                let chainspec = Command::new(binary_path.as_ref())
                    .arg("build-spec")
                    .arg("--chain")
                    .arg("dev")
                    .env_remove("RUST_LOG")
                    .run_and_get_output()
                    .context("Failed to build the chainspec")
                    .and_then(|output| {
                        serde_json::from_str::<Value>(&output.stdout)
                            .context("Failed to deserialize output as chainspec JSON")
                    })?;
                entry.insert(chainspec.clone());
                chainspec
            }
        };
        inject_wallet_balances(&mut chainspec, wallet)
            .context("Failed to add the pre-funded accounts")?;

        Ok(chainspec)
    }
}

impl NodeConfiguration for ReviveDevNode {
    fn id(&self) -> usize {
        self.id
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn eth_provider_url(&self) -> NodeUrlCollection<'_> {
        NodeUrlCollection::new().with_http_url(self.eth_rpc_process.url())
    }

    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>> {
        Some(NodeUrlCollection::new().with_ws_url(self.revive_dev_node_process.url()))
    }
}
