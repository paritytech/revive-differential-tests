use crate::internal_prelude::*;

#[derive(Debug)]
pub struct LighthouseGethNode {
    id: u32,
    process: LighthouseNodeProcess,
    wallet: Arc<EthereumWallet>,
    nonce_manager: CachedNonceManager,
    gas_filler: FallbackGasFiller,
    ws_provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    ws_subscriptions_provider: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    _directories: NodeDirectories,
}

impl LighthouseGethNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasKurtosisConfiguration,
        use_fallback_gas_filler: bool,
    ) -> Result<Self> {
        let workdir_config = context.as_working_directory_configuration();
        let wallet_config = context.as_wallet_configuration();
        let kurtosis_config = context.as_kurtosis_configuration();

        let id = NodeId::for_node("lighthouse");
        let directories = NodeDirectories::new(
            workdir_config.working_directory.as_path(),
            "lighthouse",
            id.0,
        )
        .context("Failed to initialize node directories")?;
        let config_path = directories.base_directory().join("config.yaml");
        let wrapper_directory = directories.base_directory().join("wrapper");
        create_dir_all(wrapper_directory.as_path())
            .context("Failed to create the wrapper directory")?;

        let wallet = wallet_config.wallet();
        Self::init_kurtosis_config_file(config_path.as_path())
            .context("Failed to initialize the config file")?;
        Self::init_kurtosis_wrapper(wrapper_directory.as_path(), wallet.as_ref())
            .context("Failed to initialize the wrapper directory")?;

        let enclave_name = format!(
            "enclave-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Must not fail")
                .as_nanos(),
            id.0
        );

        let process = LighthouseNodeProcess::new(
            kurtosis_config.path.as_path(),
            enclave_name.as_str(),
            wrapper_directory.as_path(),
            config_path.as_path(),
            directories.logs_directory(),
            kurtosis_config.start_timeout_ms,
        )
        .inspect_err(|err| error!(error = ?err, "Failed to spawn lighthouse"))?;

        Ok(Self {
            id: id.0,
            process,
            wallet,
            nonce_manager: Default::default(),
            gas_filler: FallbackGasFiller::new().with_fallback_mechanism(use_fallback_gas_filler),
            ws_provider: Default::default(),
            ws_subscriptions_provider: Default::default(),
            _directories: directories,
        })
    }

    fn init_kurtosis_config_file(config_file_path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(
            config_file_path.as_ref(),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/lighthouse-config.yaml"
            )),
        )
        .context("Failed to write the config yaml file")
    }

    fn init_kurtosis_wrapper(
        wrapper_directory: impl AsRef<Path>,
        wallet: &EthereumWallet,
    ) -> Result<()> {
        let accounts_file = wrapper_directory.as_ref().join("prefunded_accounts.json");
        let kurtosis_yml = wrapper_directory.as_ref().join("kurtosis.yml");
        let main_star = wrapper_directory.as_ref().join("main.star");

        let balances = NetworkWallet::<Ethereum>::signer_addresses(wallet)
            .map(|address| (address, GenesisAccount::default().with_balance(U256::MAX)))
            .collect::<BTreeMap<_, _>>();
        File::create(accounts_file)
            .context("Failed to open the prefunded accounts file")
            .map(BufWriter::new)
            .and_then(|writer| {
                serde_json::to_writer(writer, &balances)
                    .context("Failed to write pre-funded balances to file")
            })?;

        std::fs::write(
            kurtosis_yml,
            "name: github.com/local/ethereum-wrapper\nreplace:\n  {}\n",
        )
        .context("Failed to write the kurtosis yaml file")?;

        std::fs::write(main_star, WRAPPER_CODE).context("Failed to write wrapper code")?;

        Ok(())
    }

    // TODO(no-genesis-export): Remove
    pub fn node_genesis(genesis: Genesis, _: &EthereumWallet) -> Genesis {
        genesis
    }

    fn construct_provider(
        &self,
        provider_cell: Arc<OnceCell<ConcreteProvider<Ethereum, Arc<EthereumWallet>>>>,
    ) -> FrameworkFuture<Result<DynProvider>> {
        let provider = provider_cell;
        let connection_string = self.connection_string().to_string();
        let gas_filler = self.gas_filler;
        let nonce_filler = NonceFiller::new(self.nonce_manager.clone());
        let wallet = self.wallet.clone();

        Box::pin(async move {
            provider
                .get_or_try_init(|| async move {
                    construct_concurrency_limited_provider::<Ethereum, _>(
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
                .map(|provider| provider.clone().erased())
        })
    }
}

impl NodeApi for LighthouseGethNode {
    fn id(&self) -> usize {
        self.id as _
    }

    fn connection_string(&self) -> &str {
        self.process.ws_url()
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn submit_transaction(
        &self,
        mut transaction: TransactionRequest,
    ) -> FrameworkFuture<Result<TxHash>> {
        // EIP-1559 adjusts the base fee per block based on how full the previous block was relative
        // to the target (50% of the gas limit). During benchmarks, blocks are consistently ~99%
        // full, which causes the base fee to grow exponentially at ~12.4% per block. With a
        // moderate gas price (e.g. 20,000 gwei), the base fee exceeds it after roughly 90 blocks,
        // making transactions un-includable. This produces an alternating pattern of one full block
        // followed by one empty block: the empty block lowers the base fee just enough for the next
        // block to be full, which raises it again, and so on — halving effective TPS.
        //
        // Setting the gas price to u128::MAX ensures the base fee cannot exceed it for any
        // practical benchmark duration (~580 blocks / ~116 minutes of sustained full blocks). The
        // accounts are funded with U256::MAX so balance is not a concern.
        transaction.set_gas_price(u128::MAX);
        let provider = self.provider();
        Box::pin(async move {
            provider
                .await
                .context("Failed to get the provider")?
                .send_transaction(transaction)
                .await
                .context("Failed to submit transaction")
                .map(|pending_transaction| *pending_transaction.tx_hash())
        })
    }

    fn provider(&self) -> FrameworkFuture<Result<DynProvider>> {
        self.construct_provider(self.ws_provider.clone())
    }

    fn subscriptions_provider(&self) -> FrameworkFuture<Result<DynProvider>> {
        self.construct_provider(self.ws_subscriptions_provider.clone())
    }
}

impl Node for LighthouseGethNode {
    #[instrument(level = "info", skip_all, fields(lighthouse_node_id = self.id))]
    fn spawn(&mut self, _: Genesis) -> Result<()> {
        Ok(())
    }
}

const WRAPPER_CODE: &str = r#"ethereum_package = import_module("github.com/ethpandaops/ethereum-package/main.star")

def run(plan, args={}):
    accounts_json = read_file("./prefunded_accounts.json")
    accounts = json.decode(accounts_json)
    if "network_params" not in args:
        args["network_params"] = {}
    args["network_params"]["additional_preloaded_contracts"] = accounts
    return ethereum_package.run(plan, args)
"#;
