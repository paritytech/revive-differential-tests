use std::{
    io::{BufRead, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use serde_json::Value;
use serde_json::json;
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;
use std::fs;

use alloy::{
    hex,
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder, ext::DebugApi},
    rpc::types::{
        TransactionReceipt,
        trace::geth::{DiffMode, GethDebugTracingOptions, PreStateConfig, PreStateFrame},
    },
};
use serde_json::Value as JsonValue;

use crate::Node;
use revive_dt_config::Arguments;
use revive_dt_node_interaction::{
    EthereumNode, trace::trace_transaction, transaction::execute_transaction,
};

static NODE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct KitchensinkNode {
    id: u32,
    substrate_binary: PathBuf,
    eth_proxy_binary: PathBuf,
    rpc_url: String,
    wallet: EthereumWallet,
    process_substrate: Option<Child>,
    process_proxy: Option<Child>,
}

impl KitchensinkNode {
    const SUBSTRATE_READY_MARKER: &str = "Running JSON-RPC server";
    const ETH_PROXY_READY_MARKER: &str = "Running JSON-RPC server";
    const BASE_SUBSTRATE_RPC_PORT: u16 = 9944;
    const BASE_PROXY_RPC_PORT: u16 = 8545;

    fn spawn_process(&mut self, genesis: String) -> anyhow::Result<()> {
        let substrate_rpc_port = Self::BASE_SUBSTRATE_RPC_PORT + self.id as u16;
        let proxy_rpc_port = Self::BASE_PROXY_RPC_PORT + self.id as u16;

        self.rpc_url = format!("http://127.0.0.1:{proxy_rpc_port}");

        // Start Substrate node
        let mut substrate_process = Command::new(&self.substrate_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(substrate_rpc_port.to_string())
            .arg("--name")
            .arg(format!("revive-kitchensink-{}", self.id))
            .arg("--rpc-methods")
            .arg("Unsafe")
            .env(
                "RUST_LOG",
                "error,evm=debug,sc_rpc_server=info,runtime::revive=debug",
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Give the node a moment to boot
        Self::wait_ready(
            &mut substrate_process,
            Self::SUBSTRATE_READY_MARKER,
            Duration::from_secs(10),
        )?;

        let mut proxy_process = Command::new(&self.eth_proxy_binary)
            .arg("--dev")
            .arg("--rpc-port")
            .arg(proxy_rpc_port.to_string())
            .arg("--node-rpc-url")
            .arg(format!("ws://127.0.0.1:{substrate_rpc_port}"))
            .env("RUST_LOG", "info,eth-rpc=debug")
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

    fn generate_chainspec_file(&self, genesis_path: &str) -> anyhow::Result<PathBuf> {
        let mut balances = self.extract_balance_from_genesis_file(genesis_path)?;

        let mut chainspec_json: JsonValue =
            serde_json::from_str(include_str!("default_chainspec.json"))?;

        let existing_balances =
            chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        let mut merged_balances: Vec<(String, u128)> = existing_balances
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

        merged_balances.append(&mut balances);

        chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"] =
            json!(merged_balances);

        let temp_path = std::env::temp_dir().join(format!("chainspec-{}.json", 1));

        let mut temp_file = std::fs::File::create(&temp_path)?;
        serde_json::to_writer_pretty(&mut temp_file, &chainspec_json)?;
        temp_file.flush()?;

        Ok(temp_path)
    }

    fn extract_balance_from_genesis_file(
        &self,
        genesis_path: &str,
    ) -> anyhow::Result<Vec<(String, u128)>> {
        let genesis_str = fs::read_to_string(genesis_path)?;
        let genesis_json: Value = serde_json::from_str(&genesis_str)?;
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
            let substrate_addr = &self.eth_to_substrate_address(eth_addr)?;
            balances.push((substrate_addr.clone(), balance));
        }
        Ok(balances)
    }

    fn eth_to_substrate_address(&self, eth_addr: &str) -> anyhow::Result<String> {
        let eth_bytes = hex::decode(eth_addr.trim_start_matches("0x"))?;
        let mut padded = [0u8; 32];
        padded[12..].copy_from_slice(&eth_bytes);
        let account_id = AccountId32::from(padded);
        // Convert to SS58 format
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
}

impl Node for KitchensinkNode {
    fn new(config: &Arguments) -> Self {
        let id = NODE_COUNT.fetch_add(1, Ordering::SeqCst);

        Self {
            id,
            substrate_binary: config.kitchensink.clone(),
            eth_proxy_binary: config.eth_proxy.clone(),
            rpc_url: String::new(),
            wallet: config.wallet(),
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
        self.spawn_process(genesis)
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
    use tempfile::tempdir;

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
    fn test_generate_chainspec() {
        // Setup: Write a minimal Geth-style genesis.json
        let test_genesis_path = std::env::temp_dir().join("test_genesis.json");
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
        fs::write(&test_genesis_path, genesis_content).expect("Failed to write test genesis");

        let dummy_node = KitchensinkNode::new(&Arguments::default());

        let result = dummy_node.generate_chainspec_file(test_genesis_path.to_str().unwrap());

        match result {
            Ok(path) => {
                println!("Chainspec file generated at: {:?}", path);
                let contents = fs::read_to_string(&path).expect("Failed to read chainspec");

                let first_eth_addr = dummy_node
                    .eth_to_substrate_address("90F8bf6A479f320ead074411a4B0e7944Ea8c9C1")
                    .unwrap();
                let second_eth_addr = dummy_node
                    .eth_to_substrate_address("Ab8483F64d9C6d1EcF9b849Ae677dD3315835cb2")
                    .unwrap();

                assert!(contents.contains(&first_eth_addr));
                assert!(contents.contains(&second_eth_addr));
            }
            Err(e) => {
                panic!("Failed to generate chainspec: {:?}", e);
            }
        }
    }

    #[test]
    fn test_parse_genesis_alloc() {
        let temp_dir = tempdir().unwrap();
        let genesis_path = temp_dir.path().join("test_genesis.json");

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

        fs::write(&genesis_path, genesis_json).unwrap();

        let node = KitchensinkNode::new(&test_config().0);

        let result = node
            .extract_balance_from_genesis_file(genesis_path.to_str().unwrap())
            .unwrap();

        let result_map: std::collections::HashMap<_, _> = result.into_iter().collect();

        assert_eq!(
            result_map.get("5C4hrfjw9DjXZTzV3NdLyqx6imk2ZYAfDdacXRwCxXmMpQdK"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM"),
            Some(&1_000_000_000_000_000_000u128)
        );

        assert_eq!(
            result_map.get("5C4hrfjw9DjXZTzV3P9UmfbQox1vWPNoGNrwq5qYrDt3ngdY"),
            Some(&123_456_789u128)
        );
    }

    #[test]
    fn test_eth_to_substrate_address() {
        let node = KitchensinkNode::new(&test_config().0);

        let cases = vec![
            (
                "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5C4hrfjw9DjXZTzV3NdLyqx6imk2ZYAfDdacXRwCxXmMpQdK",
            ),
            (
                "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
                "5C4hrfjw9DjXZTzV3NdLyqx6imk2ZYAfDdacXRwCxXmMpQdK",
            ),
            (
                "0x0000000000000000000000000000000000000000",
                "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM",
            ),
            (
                "0xffffffffffffffffffffffffffffffffffffffff",
                "5C4hrfjw9DjXZTzV3P9UmfbQox1vWPNoGNrwq5qYrDt3ngdY",
            ),
        ];

        for (eth_addr, expected_ss58) in cases {
            let result = node.eth_to_substrate_address(eth_addr).unwrap();
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
