use crate::internal_prelude::*;

#[derive(Debug)]
pub struct ZombienetNode {
    id: usize,
    eth_rpc_process: EthRpcProcess,
    zombienet_process: ZombienetProcess,
    _directories: NodeDirectories,
}

impl ZombienetNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasZombienetConfiguration,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let wallet_config = context.as_wallet_configuration();
        let zombienet_config = context.as_zombienet_configuration();
        let rpc_config = context.as_eth_rpc_configuration();

        let id = NodeId::for_node("zombienet");
        let instance_id = Self::instance_id(id)?;
        let directories = NodeDirectories::new(
            workdir_config.working_directory.as_path(),
            "zombienet",
            instance_id.as_str(),
        )
        .context("Failed to initialize node directories")?;

        let wallet = wallet_config.wallet();
        let network_config = Self::init_zombienet_config(
            zombienet_config
                .config_path
                .as_ref()
                .context("Zombienet requires a config path")?,
            &wallet,
            instance_id.as_str(),
            directories.base_directory(),
        )
        .context("Failed to initialize the zombienet config")?;

        let zombienet_process =
            ZombienetProcess::new(network_config, zombienet_config.block_production_timeout_ms)
                .inspect_err(|err| error!(error = ?err, "Failed to spawn zombienet"))?;

        let eth_rpc_process = EthRpcProcess::new(
            rpc_config.path.as_path(),
            directories.logs_directory(),
            zombienet_process.url(),
            rpc_config.logging_level.as_str(),
            rpc_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn eth-rpc"))?;

        Ok(Self {
            id: id.0,
            eth_rpc_process,
            zombienet_process,
            _directories: directories,
        })
    }

    fn instance_id(id: NodeId) -> Result<String> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time is before UNIX epoch")?
            .as_nanos();

        Ok(format!("{}-{timestamp}-{}", std::process::id(), id.0))
    }

    fn init_zombienet_config(
        source_config_path: impl AsRef<Path>,
        wallet: &EthereumWallet,
        instance_id: impl AsRef<str>,
        base_directory: impl AsRef<Path>,
    ) -> Result<NetworkConfig> {
        let source_config = std::fs::read_to_string(source_config_path.as_ref())
            .context("Failed to read zombienet config file")?;
        let mut config = toml::from_str::<toml::Value>(&source_config)
            .context("Failed to parse zombienet TOML")?;

        Self::inject_prefunded_chainspec(&mut config, wallet, base_directory.as_ref())
            .context("Failed to inject prefunded chainspec")?;
        Self::make_node_names_unique(&mut config, instance_id);

        let modified_config_path = base_directory.as_ref().join("zombienet.toml");
        std::fs::write(
            modified_config_path.as_path(),
            toml::to_string(&config).context("Failed to serialize modified TOML")?,
        )
        .context("Failed to write modified zombienet config")?;

        NetworkConfig::load_from_toml(
            modified_config_path
                .to_str()
                .context("Modified zombienet config path is not valid UTF-8")?,
        )
        .context("Failed to load zombienet config")
    }

    fn inject_prefunded_chainspec(
        config: &mut toml::Value,
        wallet: &EthereumWallet,
        base_directory: &Path,
    ) -> Result<()> {
        let parachain = config
            .get_mut("parachains")
            .and_then(toml::Value::as_array_mut)
            .and_then(|parachains| parachains.first_mut())
            .context("No [[parachains]] found in zombienet config")?;

        let chain_name = parachain
            .get("chain")
            .and_then(toml::Value::as_str)
            .context("No chain name found in the parachain config")?;
        let command = parachain
            .get("chain_spec_command")
            .and_then(toml::Value::as_str)
            .context("No chain_spec_command found in the parachain config")?
            .replace("{{chainName}}", chain_name);

        let chainspec_path = base_directory.join("chainspec.json");
        let mut chainspec = Self::chainspec_from_command(command.as_str())?;
        inject_wallet_balances(&mut chainspec, wallet)
            .context("Failed to add the pre-funded accounts")?;
        File::create(chainspec_path.as_path())
            .context("Failed to create the chainspec file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &chainspec)
                    .context("Failed to write chainspec to file")
            })?;

        let table = parachain
            .as_table_mut()
            .context("Parachain config is not a table")?;
        table.remove("chain_spec_command");
        table.insert(
            "chain_spec_path".to_string(),
            toml::Value::String(chainspec_path.to_string_lossy().into_owned()),
        );

        Ok(())
    }

    fn chainspec_from_command(command: impl AsRef<str>) -> Result<Value> {
        Command::new("sh")
            .arg("-c")
            .arg(command.as_ref())
            .env_remove("RUST_LOG")
            .run_and_get_output()
            .context("Failed to run chain_spec_command")
            .and_then(|output| {
                serde_json::from_str::<Value>(&output.stdout)
                    .context("Failed to parse chainspec JSON")
            })
    }

    fn make_node_names_unique(config: &mut toml::Value, instance_id: impl AsRef<str>) {
        let suffix = format!("-{}", instance_id.as_ref());

        if let Some(nodes) = config
            .get_mut("relaychain")
            .and_then(|relaychain| relaychain.get_mut("nodes"))
            .and_then(toml::Value::as_array_mut)
        {
            Self::suffix_node_names(nodes, suffix.as_str());
        }

        if let Some(parachains) = config
            .get_mut("parachains")
            .and_then(toml::Value::as_array_mut)
        {
            for parachain in parachains {
                if let Some(collators) = parachain
                    .get_mut("collators")
                    .and_then(toml::Value::as_array_mut)
                {
                    Self::suffix_node_names(collators, suffix.as_str());
                }
            }
        }
    }

    fn suffix_node_names(nodes: &mut [toml::Value], suffix: &str) {
        for node in nodes {
            let Some(table) = node.as_table_mut() else {
                continue;
            };
            let name = table
                .get("name")
                .and_then(toml::Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(name) = name {
                table.insert(
                    "name".into(),
                    toml::Value::String(format!("{name}{suffix}")),
                );
            }
        }
    }

    pub fn node_genesis(node_path: impl AsRef<Path>, wallet: &EthereumWallet) -> Result<Value> {
        let mut chainspec_json = Command::new(node_path.as_ref())
            .arg("build-spec")
            .arg("--chain")
            .arg("asset-hub-westend-local")
            .env_remove("RUST_LOG")
            .run_and_get_output()
            .context("Failed to export the chainspec of the chain")
            .and_then(|output| {
                serde_json::from_str::<Value>(&output.stdout)
                    .context("Failed to parse Substrate chain spec JSON")
            })?;

        inject_wallet_balances(&mut chainspec_json, wallet)?;

        Ok(chainspec_json)
    }
}

impl NodeConfiguration for ZombienetNode {
    fn id(&self) -> usize {
        self.id
    }

    fn configurations(&self) -> NodeConnectorConfiguration {
        NodeConnectorConfiguration {
            hooks: Some(NodeConnectorHooks {
                pre_submission_hook: Some(PreSubmissionHook::Disabled),
            }),
            substrate_provider_configuration: Some(SubstrateProviderConfiguration {
                submission_concurrency_configuration: Some(
                    ProviderConcurrencyConfiguration::SemaphoreBasedLimiter { permits: 500 },
                ),
            }),
            ..Default::default()
        }
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn eth_provider_url(&self) -> NodeUrlCollection<'_> {
        NodeUrlCollection::new().with_http_url(self.eth_rpc_process.url())
    }

    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>> {
        Some(NodeUrlCollection::new().with_ws_url(self.zombienet_process.url()))
    }
}
