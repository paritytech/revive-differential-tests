use alloy::{
    genesis::{Genesis, GenesisAccount},
    network::{Ethereum, NetworkWallet},
    primitives::{Address, U256},
};
use anyhow::{Context, Result};
use serde_json::{Value as JsonValue, json};
use sp_core::crypto::Ss58Codec;
use sp_runtime::AccountId32;
use std::{fs::File, path::Path, process::Command};

pub fn export_and_patch_chainspec_json(
    binary_path: &Path,
    export_command: &str,
    chain_arg: &str,
    output_path: &Path,
    genesis: &mut Genesis,
    wallet: &impl NetworkWallet<Ethereum>,
    initial_balance: u128,
) -> Result<()> {
    // Note: we do not pipe the logs of this process to a separate file since this is just a
    // once-off export of the default chain spec and not part of the long-running node process.
    let output = Command::new(binary_path)
        .arg(export_command)
        .arg("--chain")
        .arg(chain_arg)
        .env_remove("RUST_LOG")
        .output()
        .context("Failed to export the chain-spec")?;

    if !output.status.success() {
        anyhow::bail!(
            "Export chain-spec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let content =
        String::from_utf8(output.stdout).context("Failed to decode chain-spec output as UTF-8")?;
    let mut chainspec_json: JsonValue =
        serde_json::from_str(&content).context("Failed to parse chain spec JSON")?;

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

    for signer_address in wallet.signer_addresses() {
        genesis
            .alloc
            .entry(signer_address)
            .or_insert(GenesisAccount::default().with_balance(U256::from(initial_balance)));
    }

    let mut eth_balances =
        crate::node_implementations::common::chainspec::extract_balance_from_genesis_file(genesis)
            .context("Failed to extract balances from EVM genesis JSON")?;

    merged_balances.append(&mut eth_balances);

    chainspec_json["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"] =
        json!(merged_balances);

    serde_json::to_writer_pretty(
        File::create(output_path).context("Failed to create chainspec file")?,
        &chainspec_json,
    )
    .context("Failed to write chainspec JSON")?;

    Ok(())
}

pub fn extract_balance_from_genesis_file(genesis: &Genesis) -> anyhow::Result<Vec<(String, u128)>> {
    genesis
        .alloc
        .iter()
        .try_fold(Vec::new(), |mut vec, (address, acc)| {
            let substrate_address = eth_to_polkadot_address(address);
            let balance = acc.balance.try_into()?;
            vec.push((substrate_address, balance));
            Ok(vec)
        })
}

pub fn eth_to_polkadot_address(address: &Address) -> String {
    let eth_bytes = address.0.0;

    let mut padded = [0xEEu8; 32];
    padded[..20].copy_from_slice(&eth_bytes);

    let account_id = AccountId32::from(padded);
    account_id.to_ss58check()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let result =
            extract_balance_from_genesis_file(&serde_json::from_str(genesis_json).unwrap())
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
    fn test_eth_to_polkadot_address() {
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
            let result = eth_to_polkadot_address(&eth_addr.parse().unwrap());
            assert_eq!(
                result, expected_ss58,
                "Mismatch for Ethereum address {eth_addr}"
            );
        }
    }

    #[test]
    fn print_eth_to_polkadot_mappings() {
        let eth_addresses = vec![
            "0x90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
            "0xffffffffffffffffffffffffffffffffffffffff",
            "90F8bf6A479f320ead074411a4B0e7944Ea8c9C1",
        ];

        for eth_addr in eth_addresses {
            let ss58 = eth_to_polkadot_address(&eth_addr.parse().unwrap());

            println!("Ethereum: {eth_addr} -> Polkadot SS58: {ss58}");
        }
    }
}
