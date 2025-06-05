use std::{
    fs::create_dir_all,
    io::BufRead,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use alloy::{
    hex,
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder, ext::DebugApi},
    rpc::types::{
        TransactionReceipt,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;

use revive_dt_config::Arguments;
use revive_dt_node_interaction::{
    EthereumNode, trace::trace_transaction, transaction::execute_transaction,
};

use crate::Node;

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct KitchensinkNode {
    id: u32,
    substrate_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    rpc_url: String,
    wallet: EthereumWallet,
    base_directory: PathBuf,
    process_substrate: Option<Child>,
    process_proxy: Option<Child>,
}

impl KitchensinkNode {
    const BASE_DIRECTORY: &str = "kitchensink";
    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const CHAIN_SPEC_JSON_FILE: &str = "template_chainspec.json";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    const SUBSTRATE_LOG_ENV: &str = "error,evm=debug,sc_rpc_server=info,runtime::revive=debug";
    const PROXY_LOG_ENV: &str = "info,eth-rpc=debug";

    fn init(&mut self, genesis: &str) -> anyhow::Result<&mut Self> {
        create_dir_all(&self.base_directory)?;

        let template_chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        let output = Command::new(&self.substrate_binary)
            .arg("export-chain-spec")
            .arg("--chain")
            .arg("dev")
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "substrate-node export-chain-spec failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let content = String::from_utf8(output.stdout)?;
        let mut chainspec_json: JsonValue = serde_json::from_str(&content)?;

        let existing_chainspec_balances =
            chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        let mut merged_balances: Vec<(String, u128)> = existing_chainspec_balances
            .into_iter()
            .filter_map(|val| {
                if let Some(arr) = val.as_array() {
                    if arr.len() == 2 {
                        let account = arr[0].as_str()?.to_string();
                        let balance = arr[1].as_f64()? as u128;
                        return Some((account, balance));
                    }
                }
                None
            })
            .collect();
        let mut eth_balances = self.extract_balance_from_genesis_file(genesis)?;
        merged_balances.append(&mut eth_balances);

        chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"] =
            json!(merged_balances);

        serde_json::to_writer_pretty(
            std::fs::File::create(&template_chainspec_path)?,
            &chainspec_json,
        )?;
        Ok(self)
    }

    fn spawn_process(&mut self) -> anyhow::Result<()> {
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        let chainspec_path = self.base_directory.join(Self::CHAIN_SPEC_JSON_FILE);

        // Start Substrate node
        let mut substrate_process = Command::new(&self.substrate_binary)
            .arg("--chain")
            .arg(chainspec_path)
            .arg("--base-path")
            .arg(&self.base_directory)
            .arg("--rpc-port")
            .arg(substrate_rpc_port.to_string())
            .arg("--name")
            .arg(format!("revive-kitchensink-{}", self.id))
            .arg("--force-authoring")
            .arg("--rpc-methods")
            .arg("Unsafe")
            .arg("--rpc-cors")
            .arg("all")
            .env("RUST_LOG", Self::SUBSTRATE_LOG_ENV)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Give the node a moment to boot
        Self::wait_ready(
            &mut substrate_process,
            Self::SUBSTRATE_READY_MARKER,
            Duration::from_secs(30),
        )?;

        let mut proxy_process = Command::new(&self.eth_proxy_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(proxy_rpc_port.to_string())
            .arg("--node-rpc-url")
            .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
            .env("RUST_LOG", Self::PROXY_LOG_ENV)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        Self::wait_ready(
            &mut proxy_process,
            Self::ETH_PROXY_READY_MARKER,
            Duration::from_secs(30),
        )?;

        self.process_substrate = Some(substrate_process);
        self.process_proxy = Some(proxy_process);

        Ok(())
    }

    fn extract_balance_from_genesis_file(
        &self,
        genesis_str: &str,
    ) -> anyhow::Result<Vec<(String, u128)>> {
        let genesis_json: JsonValue = serde_json::from_str(genesis_str)?;
        let alloc = genesis_json
            .get("alloc")
            .and_then(|a| a.as_object())
            .ok_or_else(|| anyhow::anyhow!("Missing 'alloc' in genesis"))?;

        let mut balances = Vec::new();
        for (eth_addr, obj) in alloc.iter() {
            let balance_str = obj.get("balance").and_then(|b| b.as_str()).unwrap_or("0");
            let balance = if balance_str.starts_with("0x") {
                u128::from_str_radix(balance_str.trim_start_matches("0x"), 16)?
            } else {
                balance_str.parse::<u128>()?
            };
            let substrate_addr = Self::eth_to_substrate_address(eth_addr)?;
            balances.push((substrate_addr.clone(), balance));
        }
        Ok(balances)
    }

    fn eth_to_substrate_address(eth_addr: &str) -> anyhow::Result<String> {
        let eth_bytes = hex::decode(eth_addr.trim_start_matches("0x"))?;
        if eth_bytes.len() != 20 {
            anyhow::bail!(
                "Invalid Ethereum address length: expected 20 bytes, got {}",
                eth_bytes.len()
            );
        }

        let mut padded = [0xEEu8; 32];
        padded[..20].copy_from_slice(&eth_bytes);

        let account_id = AccountId32::from(padded);
        Ok(account_id.to_ss58check())
    }

    fn wait_ready(child: &mut Child, marker: &str, timeout: Duration) -> anyhow::Result<()> {
        let start_time = std::time::Instant::now();
        let stderr = child.stderr.take().expect("stderr must be piped");

        let mut lines = std::io::BufReader::new(stderr).lines();
        loop {
            if let Some(Ok(line)) = lines.next() {
                println!("Kitchensink log: {line:?}");
                if line.contains(marker) {
                    std::thread::spawn(move || for _ in lines.by_ref() {});
                    return Ok(());
                }
            }

            if start_time.elapsed() > timeout {
                let _ = child.kill();
                anyhow::bail!("Timeout waiting for process readiness: {marker}");
            }
        }
    }

    pub fn eth_rpc_version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.eth_proxy_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }
}

impl EthereumNode for KitchensinkNode {
    fn execute_transaction(
        &self,
        transaction: alloy::rpc::types::TransactionRequest,
    ) -> anyhow::Result<TransactionReceipt> {
        let url = self.rpc_url.clone();
        let wallet = self.wallet.clone();

        execute_transaction(Box::pin(async move {
            Ok(ProviderBuilder::new()
                .wallet(wallet)
                .connect(&url)
                .await?
                .send_transaction(transaction)
                .await?
                .get_receipt()
                .await?)
        }))
    }

    fn trace_transaction(
        &self,
        transaction: TransactionReceipt,
    ) -> anyhow::Result<alloy::rpc::types::trace::geth::GethTrace> {
        let url = self.rpc_url.clone();
        let trace_options = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
            diff_mode: Some(true),
            disable_code: None,
            disable_storage: None,
        });

        let wallet = self.wallet.clone();

        trace_transaction(Box::pin(async move {
            Ok(ProviderBuilder::new()
                .wallet(wallet)
                .connect(&url)
                .await?
                .debug_trace_transaction(transaction.transaction_hash, trace_options)
                .await?)
        }))
    }

    fn state_diff(&self, transaction: TransactionReceipt) -> anyhow::Result<DiffMode> {
        match self
            .trace_transaction(transaction)?
            .try_into_pre_state_frame()?
        {
            PreStateFrame::Diff(diff) => Ok(diff),
            _ => anyhow::bail!("expected a diff mode trace"),
        }
    }
}

impl Node for KitchensinkNode {
    fn new(config: &Arguments) -> Self {
        let kitchensink_directory = config.directory().join(Self::BASE_DIRECTORY);
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);
        let base_directory = kitchensink_directory.join(id.to_string());

        Self {
            id,
            substrate_binary: config.kitchensink.clone(),
            eth_proxy_binary: config.eth_proxy.clone(),
            rpc_url: String::new(),
            wallet: config.wallet(),
            base_directory,
            process_substrate: None,
            process_proxy: None,
        }
    }

    fn connection_string(&self) -> String {
        self.rpc_url.clone()
    }

    fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.process_proxy.take() {
            let _ = child.kill();
        }
        if let Some(mut child) = self.process_substrate.take() {
            let _ = child.kill();
        }
        Ok(())
    }

    fn spawn(&mut self, genesis: String) -> anyhow::Result<()> {
        self.init(&genesis)?.spawn_process()
    }

    fn version(&self) -> anyhow::Result<String> {
        let output = Command::new(&self.substrate_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?
            .wait_with_output()?
            .stdout;
        Ok(String::from_utf8_lossy(&output).into())
    }
}

impl Drop for KitchensinkNode {
    fn drop(&mut self) {
        if let Some(mut child) = self.process_proxy.take() {
            let _ = child.kill();
        }
        if let Some(mut child) = self.process_substrate.take() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use revive_dt_config::Arguments;
    use std::path::PathBuf;
    use temp_dir::TempDir;

    use std::fs;

    use super::KitchensinkNode;
    use crate::{GENESIS_JSON, Node};

    fn test_config() -> (Arguments, TempDir) {
        let mut config = Arguments::default();
        let temp_dir = TempDir::new().unwrap();

        config.working_directory = temp_dir.path().to_path_buf().into();

        config.kitchensink = PathBuf::from("substrate-node");
        config.eth_proxy = PathBuf::from("eth-rpc");

        (config, temp_dir)
    }

    #[test]
    fn test_init_generates_chainspec_with_balances() {
        let genesis_content = r#"
        {
            "alloc": {
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1": {
                    "balance": "1000000000000000000"
                },
                "Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2": {
                    "balance": "2000000000000000000"
                }
            }
        }
        "#;

        let mut dummy_node = KitchensinkNode::new(&test_config().0);

        // Call `init()`
        dummy_node.init(genesis_content).expect("init failed");

        // Check that the patched chainspec file was generated
        let final_chainspec_path = dummy_node
            .base_directory
            .join(KitchensinkNode::CHAIN_SPEC_JSON_FILE);
        assert!(final_chainspec_path.exists(), "Chainspec file should exist");

        let contents = fs::read_to_string(&final_chainspec_path).expect("Failed to read chainspec");

        // Validate that the Substrate addresses derived from the Ethereum addresses are in the file
        let first_eth_addr =
            KitchensinkNode::eth_to_substrate_address("90F8bf6A479f320ead074411a4B0e7944Ea8c9C1")
                .unwrap();
        let second_eth_addr =
            KitchensinkNode::eth_to_substrate_address("Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2")
                .unwrap();

        assert!(
            contents.contains(&first_eth_addr),
            "Chainspec should contain Substrate address for first Ethereum account"
        );
        assert!(
            contents.contains(&second_eth_addr),
            "Chainspec should contain Substrate address for second Ethereum account"
        );
    }

    #[test]
    fn test_parse_genesis_alloc() {
        // Create test genesis file
        let genesis_json = r#"
        {
          "alloc": {
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1": { "balance": "1000000000000000000" },
            "0x0000000000000000000000000000000000000000": { "balance": "0xDE0B6B3A7640000" },
            "0xffffffffffffffffffffffffffffffffffffffff": { "balance": "123456789" }
          }
        }
        "#;

        let node = KitchensinkNode::new(&test_config().0);

        let result = node
            .extract_balance_from_genesis_file(genesis_json)
            .unwrap();

        let result_map: std::collections::HashMap<_, _> = result.into_iter().collect();

        assert_eq!(
            result_map.get("5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5C4hrfjw9DjXZTzV3MwzrrAr9P1MLDHajjSidz9bR544LEq1"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5HrN7fHLXWcFiXPwwtq2EkSGns9eMmoUQnbVKweNz3VVr6N4"),
            Some(&123_456_789u128)
        );
    }

    #[test]
    fn print_eth_to_substrate_mappings() {
        let eth_addresses = vec![
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
            "0xffffffffffffffffffffffffffffffffffffffff",
            "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
        ];

        for eth_addr in eth_addresses {
            let ss58 = KitchensinkNode::eth_to_substrate_address(eth_addr).unwrap();

            println!("Ethereum: {eth_addr} -> Substrate SS58: {ss58}");
        }
    }

    #[test]
    fn test_eth_to_substrate_address() {
        let cases = vec![
            (
                "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5FLneRcWAfk3X3tg6PuGyLNGAquPAZez5gpqvyuf3yUK8VaV",
            ),
            (
                "0x0000000000000000000000000000000000000000",
                "5C4hrfjw9DjXZTzV3MwzrrAr9P1MLDHajjSidz9bR544LEq1",
            ),
            (
                "0xffffffffffffffffffffffffffffffffffffffff",
                "5HrN7fHLXWcFiXPwwtq2EkSGns9eMmoUQnbVKweNz3VVr6N4",
            ),
        ];

        for (eth_addr, expected_ss58) in cases {
            let result = KitchensinkNode::eth_to_substrate_address(eth_addr).unwrap();
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    fn spawn_works() {
        let (config, _temp_dir) = test_config();

        let mut node = KitchensinkNode::new(&config);
        node.spawn(GENESIS_JSON.to_string()).unwrap();
    }

    #[test]
    fn version_works() {
        let (config, _temp_dir) = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.version().unwrap();

        assert!(
            version.starts_with("substrate-node"),
            "Expected substrate-node version string, got: {version}"
        );
    }

    #[test]
    fn eth_rpc_version_works() {
        let (config, _temp_dir) = test_config();

        let node = KitchensinkNode::new(&config);
        let version = node.eth_rpc_version().unwrap();

        assert!(
            version.starts_with("pallet-revive-eth-rpc"),
            "Expected eth-rpc version string, got: {version}"
        );
    }
}
