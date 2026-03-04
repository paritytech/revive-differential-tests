use crate::internal_prelude::*;

/// Converts an Ethereum [`Address`] to a Polkadot SS58 address by padding the
/// 20-byte Ethereum address into a 32-byte `AccountId32` (filled with `0xEE`).
pub fn eth_to_polkadot_address(address: &Address) -> String {
    let eth_bytes = address.0.0;

    let mut padded = [0xEEu8; 32];
    padded[..20].copy_from_slice(&eth_bytes);

    let account_id = AccountId32::from(padded);
    account_id.to_ss58check()
}

/// Appends wallet balances to the chainspec JSON's
/// `genesis.runtimeGenesis.patch.balances.balances` array.
///
/// Each signer address is converted to its Polkadot SS58 representation and
/// added with [`INITIAL_BALANCE`].
pub fn inject_wallet_balances(
    chainspec: &mut serde_json::Value,
    wallet: &EthereumWallet,
) -> anyhow::Result<()> {
    let balances = chainspec["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
        .as_array_mut()
        .context("Failed to find balances array in chainspec")?;

    for address in NetworkWallet::<Ethereum>::signer_addresses(wallet) {
        let substrate_address = eth_to_polkadot_address(&address);
        balances.push(json!((substrate_address, INITIAL_BALANCE)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
