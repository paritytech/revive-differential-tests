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
        working_directory_configuration: &WorkingDirectoryConfiguration,
        eth_rpc_configuration: &EthRpcConfiguration,
        wallet_configuration: &WalletConfiguration,
        revive_dev_node_configuration: &ReviveDevNodeConfiguration,
    ) -> Result<Self> {
        let id = NodeId::for_node("revive-dev-node");
        let directories = NodeDirectories::new(
            working_directory_configuration.working_directory.as_path(),
            "revive-dev-node",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let chainspec_path = directories.base_directory().join("chainspec.json");

        let wallet = wallet_configuration.wallet();
        Self::init_chainspec(
            revive_dev_node_configuration.path.as_path(),
            &wallet,
            chainspec_path.as_path(),
        )
        .context("Failed to initialize the chainspec file")?;

        let revive_dev_node_process = ReviveDevNodeProcess::new(
            revive_dev_node_configuration.path.as_path(),
            chainspec_path,
            revive_dev_node_configuration.consensus.as_str(),
            directories.data_directory(),
            directories.logs_directory(),
            revive_dev_node_configuration.logging_level.as_str(),
            &revive_dev_node_configuration.environment_variables,
            revive_dev_node_configuration.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn revive-dev-node"))?;

        let eth_rpc_process = EthRpcProcess::new(
            eth_rpc_configuration.path.as_path(),
            directories.logs_directory(),
            revive_dev_node_process.url(),
            eth_rpc_configuration.logging_level.as_str(),
            &eth_rpc_configuration.environment_variables,
            eth_rpc_configuration.start_timeout_ms,
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

    fn configurations(&self) -> NodeConnectorConfiguration {
        NodeConnectorConfiguration {
            behaviors: Some(NodeConnectorBehaviors {
                submission_behavior: Some(SubmissionBehavior::UseSubstrateRpcAndAwaitInclusion),
            }),
            hooks: Some(NodeConnectorHooks {
                pre_submission_hook: Some(PreSubmissionHook::MaxGasPrice),
            }),
            ..Default::default()
        }
    }

    fn eth_provider_url(&self) -> NodeUrlCollection<'_> {
        NodeUrlCollection::new().with_http_url(self.eth_rpc_process.url())
    }

    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>> {
        Some(NodeUrlCollection::new().with_ws_url(self.revive_dev_node_process.url()))
    }
}
