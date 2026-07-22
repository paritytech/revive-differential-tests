use crate::internal_prelude::*;

static DEFAULT_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    static GENESIS: &str = include_str!("../../../../assets/dev-genesis.json");
    serde_json::from_str(GENESIS).expect("qed; genesis is valid JSON")
});

#[derive(Debug)]
pub struct GethNode {
    id: usize,
    process: GethProcess,
    _directories: NodeDirectories,
}

impl GethNode {
    pub fn new(
        working_directory_configuration: &WorkingDirectoryConfiguration,
        wallet_configuration: &WalletConfiguration,
        geth_configuration: &GethConfiguration,
    ) -> Result<Self> {
        let id = NodeId::for_node("geth");
        let directories = NodeDirectories::new(
            working_directory_configuration.working_directory.as_path(),
            "geth",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let ipc_path = directories.base_directory().join("geth.ipc");
        let genesis_path = directories.base_directory().join("genesis.json");

        let wallet = wallet_configuration.wallet();
        let mut genesis = DEFAULT_GENESIS.clone();
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
            geth_configuration.path.as_path(),
            genesis_path,
            ipc_path.as_path(),
            directories.data_directory(),
            directories.logs_directory(),
            geth_configuration.logging_level.as_str(),
            &geth_configuration.environment_variables,
            geth_configuration.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn geth"))?;

        Ok(Self {
            id: id.0,
            process,
            _directories: directories,
        })
    }

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
