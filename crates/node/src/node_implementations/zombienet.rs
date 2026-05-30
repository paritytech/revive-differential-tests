use crate::internal_prelude::*;

#[derive(Debug)]
pub struct ZombienetNode {
    id: u32,
    eth_rpc_process: EthRpcProcess,
    zombienet_process: ZombienetProcess,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    gas_filler: FallbackGasFiller,
    provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    substrate_provider: Arc<OnceCell<OnlineClient<PolkadotConfig>>>,
    submission_semaphore: Arc<Semaphore>,
    _directories: NodeDirectories,
}

impl ZombienetNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasEthRpcConfiguration
        + HasWalletConfiguration
        + HasZombienetConfiguration,
        use_fallback_gas_filler: bool,
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
            wallet,
            nonce_manager: CachedNonceManager::default(),
            gas_filler: FallbackGasFiller::new().with_fallback_mechanism(use_fallback_gas_filler),
            provider: Default::default(),
            substrate_provider: Default::default(),
            submission_semaphore: Arc::new(Semaphore::new(1000)),
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

    fn provider(&self) -> FrameworkFuture<Result<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>> {
        let provider = self.provider.clone();
        let connection_string = self.connection_string().to_string();
        let gas_filler = self.gas_filler;
        let nonce_filler = NonceFiller::new(self.nonce_manager.clone());
        let wallet = self.wallet.clone();

        Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    construct_concurrency_limited_provider(
                        &connection_string,
                        gas_filler,
                        ChainIdFiller::default(),
                        nonce_filler,
                        wallet,
                    )
                    .await
                    .context("Failed to construct the provider")
                })
                .await
                .cloned()
        })
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

impl NodeApi for ZombienetNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.eth_rpc_process.url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn submit_transaction(
        &self,
        mut transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<TxHash>> {
        transaction.set_gas_price(u128::MAX);

        let provider = self.provider();
        let substrate_provider = NodeApi::substrate_provider(self);
        let semaphore = self.submission_semaphore.clone();

        Box::pin(async move {
            let provider = provider.await.context("Failed to get the provider")?;
            let substrate_provider = substrate_provider
                .expect("qed; this is a substrate node")
                .await
                .context("Failed to get the provider")?;

            let signed_transaction = provider
                .fill(transaction)
                .await
                .context("Failed to fill transaction")?
                .try_into_envelope()
                .context("Failed to construct envelope from filled transaction")?;
            let tx_hash = signed_transaction.tx_hash();
            let payload = signed_transaction.encoded_2718();

            let call = revive_metadata::tx()
                .revive()
                .eth_transact(payload.to_vec());
            let _guard = semaphore
                .acquire_owned()
                .await
                .context("Failed to acquire permit")?;
            substrate_provider
                .tx()
                .create_unsigned(&call)
                .context("Failed to create an unsigned transaction")?
                .submit()
                .await
                .context("Failed to submit the transaction through subxt")?;

            Ok(*tx_hash)
        })
    }

    fn provider(&self) -> FrameworkFuture<Result<DynProvider>> {
        Box::pin(
            self.provider()
                .map(|provider| provider.map(|provider| provider.erased())),
        )
    }

    fn substrate_provider(&self) -> Option<FrameworkFuture<Result<OnlineClient<PolkadotConfig>>>> {
        let provider = self.substrate_provider.clone();
        let connection_string = self.zombienet_process.url().to_string();

        Some(Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    OnlineClient::from_url(connection_string)
                        .await
                        .context("Failed to create a new online client")
                })
                .await
                .cloned()
        }))
    }

    fn substrate_rpc_client(
        &self,
    ) -> Option<FrameworkFuture<Result<subxt::backend::rpc::RpcClient>>> {
        let connection_string = self.zombienet_process.url().to_string();

        Some(Box::pin(async move {
            subxt::backend::rpc::RpcClient::from_insecure_url(connection_string)
                .await
                .context("Failed to create the substrate RPC client")
        }))
    }
}

impl Node for ZombienetNode {
    fn spawn(&mut self, _: Genesis) -> Result<()> {
        Ok(())
    }
}
