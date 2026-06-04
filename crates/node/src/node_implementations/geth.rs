use crate::internal_prelude::*;

#[derive(Debug)]
pub struct GethNode {
    id: usize,
    process: GethProcess,
    _directories: NodeDirectories,
}

impl GethNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasGethConfiguration
        + HasGenesisConfiguration
        + Clone,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let genesis_config = context.as_genesis_configuration();
        let wallet_config = context.as_wallet_configuration();
        let geth_config = context.as_geth_configuration();

        let id = NodeId::for_node("geth");
        let directories =
            NodeDirectories::new(workdir_config.working_directory.as_path(), "geth", id.0)
                .context("Failed to initialize node directories")?;
        let ipc_path = directories.base_directory().join("geth.ipc");
        let genesis_path = directories.base_directory().join("genesis.json");

        let wallet = wallet_config.wallet();
        let mut genesis = genesis_config
            .genesis()
            .context("Failed to get genesis for geth")?
            .clone();
        for signer_address in NetworkWallet::<Ethereum>::signer_addresses(&wallet) {
            genesis
                .alloc
                .entry(signer_address)
                .or_insert(GenesisAccount::default().with_balance(U256::from(INITIAL_BALANCE)));
        }
        File::create(genesis_path.as_path())
            .context("Failed to create the genesis path file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &genesis).context("Failed to write genesis to file")
            })?;

        let process = GethProcess::new(
            geth_config.path.as_path(),
            genesis_path,
            ipc_path.as_path(),
            directories.data_directory(),
            directories.logs_directory(),
            geth_config.logging_level.as_str(),
            geth_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn geth"))?;

        Ok(Self {
            id: id.0,
            process,
            _directories: directories,
        })
    }

    // TODO(no-genesis-export): Remove this function
    pub fn node_genesis(genesis: Genesis, _: &EthereumWallet) -> Genesis {
        genesis
    }
}

impl NodeConfiguration for GethNode {
    fn id(&self) -> usize {
        self.id
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn eth_provider_url(&self) -> NodeUrlCollection<'_> {
        NodeUrlCollection::new().with_ipc_url(self.process.url())
    }

    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>> {
        None
    }
}
