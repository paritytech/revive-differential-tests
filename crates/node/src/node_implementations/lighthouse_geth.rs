use crate::internal_prelude::*;

#[derive(Debug)]
pub struct LighthouseGethNode {
    id: usize,
    process: LighthouseNodeProcess,
    _directories: NodeDirectories,
}

impl LighthouseGethNode {
    pub fn new(
        context: impl HasWorkingDirectoryConfiguration
        + HasWalletConfiguration
        + HasKurtosisConfiguration,
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

    pub fn node_genesis(genesis: Genesis, _: &EthereumWallet) -> Genesis {
        genesis
    }
}

impl NodeConfiguration for LighthouseGethNode {
    fn id(&self) -> usize {
        self.id
    }

    fn configurations(&self) -> NodeConnectorConfiguration {
        NodeConnectorConfiguration {
            hooks: Some(NodeConnectorHooks {
                pre_submission_hook: Some(PreSubmissionHook::MaxGasPrice),
            }),
            ..Default::default()
        }
    }

    fn evm_version(&self) -> EVMVersion {
        EVMVersion::Cancun
    }

    fn eth_provider_url(&self) -> NodeUrlCollection<'_> {
        NodeUrlCollection::new()
            .with_ws_url(self.process.ws_url())
            .with_http_url(self.process.http_url())
    }

    fn substrate_provider_url(&self) -> Option<NodeUrlCollection<'_>> {
        None
    }
}

const WRAPPER_CODE: &str = r#"ethereum_package = import_module("github.com/ethpandaops/ethereum-package/main.star@6.1.0")

def run(plan, args={}):
    accounts_json = read_file("./prefunded_accounts.json")
    accounts = json.decode(accounts_json)
    if "network_params" not in args:
        args["network_params"] = {}
    args["network_params"]["additional_preloaded_contracts"] = accounts
    return ethereum_package.run(plan, args)
"#;
