use crate::internal_prelude::*;

#[derive(Debug)]
pub struct PolkadotOmnichainNode {
    id: usize,
    eth_rpc_process: EthRpcProcess,
    polkadot_omnichain_node_process: PolkadotOmniNodeProcess,
    _directories: NodeDirectories,
}

impl PolkadotOmnichainNode {
    pub fn new(
        working_directory_configuration: &WorkingDirectoryConfiguration,
        eth_rpc_configuration: &EthRpcConfiguration,
        wallet_configuration: &WalletConfiguration,
        polkadot_omnichain_node_configuration: &PolkadotOmnichainNodeConfiguration,
    ) -> Result<Self> {
        let source_chainspec_path = polkadot_omnichain_node_configuration
            .chain_spec_path
            .as_ref()
            .context("No chain spec path provided for the polkadot-omni-node")?;
        polkadot_omnichain_node_configuration
            .parachain_id
            .context("No argument provided for the parachain-id")?;

        let id = NodeId::for_node("polkadot-omni-node");
        let directories = NodeDirectories::new(
            working_directory_configuration.working_directory.as_path(),
            "polkadot-omni-node",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let chainspec_path = directories.base_directory().join("chainspec.json");

        let wallet = wallet_configuration.wallet();
        Self::init_chainspec(&wallet, source_chainspec_path, chainspec_path.as_path())
            .context("Failed to initialize the chainspec file")?;

        let polkadot_omnichain_node_process = PolkadotOmniNodeProcess::new(
            polkadot_omnichain_node_configuration.path.as_path(),
            polkadot_omnichain_node_configuration.block_time_ms,
            chainspec_path,
            directories.data_directory(),
            directories.logs_directory(),
            polkadot_omnichain_node_configuration.logging_level.as_str(),
            &polkadot_omnichain_node_configuration.environment_variables,
            polkadot_omnichain_node_configuration.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn polkadot-omni-node"))?;

        let eth_rpc_process = EthRpcProcess::new(
            eth_rpc_configuration.path.as_path(),
            directories.logs_directory(),
            polkadot_omnichain_node_process.url(),
            eth_rpc_configuration.logging_level.as_str(),
            &eth_rpc_configuration.environment_variables,
            eth_rpc_configuration.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?;

        Ok(Self {
            id: id.0,
            eth_rpc_process,
            polkadot_omnichain_node_process,
            _directories: directories,
        })
    }

    fn init_chainspec(
        wallet: &EthereumWallet,
        chain_spec_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
    ) -> Result<()> {
        let chainspec =
            Self::chainspec(wallet, chain_spec_path).context("Failed to create the chainspec")?;
        File::create(output_path.as_ref())
            .context("Failed to create the chainspec file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &chainspec)
                    .context("Failed to serialize chainspec to writer")
            })?;
        Ok(())
    }

    pub fn chainspec(wallet: &EthereumWallet, chain_spec_path: impl AsRef<Path>) -> Result<Value> {
        let mut chainspec = File::open(chain_spec_path.as_ref())
            .context("Failed to open the chainspec file")
            .map(BufReader::new)
            .and_then(|reader| {
                serde_json::from_reader::<_, Value>(reader)
                    .context("Failed to deserialize chainspec file")
            })?;
        inject_wallet_balances(&mut chainspec, wallet)?;
        Ok(chainspec)
    }
}

impl NodeConfiguration for PolkadotOmnichainNode {
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
        Some(NodeUrlCollection::new().with_ws_url(self.polkadot_omnichain_node_process.url()))
    }
}
