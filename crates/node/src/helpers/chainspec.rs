use crate::internal_prelude::*;

pub fn eth_to_polkadot_address(address: &Address) -> String {
    let eth_bytes = address.0.0;

    let mut padded = [0xEEu8; 32];
    padded[..20].copy_from_slice(&eth_bytes);

    let account_id = AccountId32::from(padded);
    account_id.to_ss58check()
}

pub fn inject_wallet_balances(
    chainspec: &mut serde_json::Value,
    wallet: &EthereumWallet,
) -> Result<()> {
    let balances = chainspec["genesis"]["runtimeGenesis"]["patch"]["balances"]["balances"]
        .as_array_mut()
        .context("Failed to find balances array in chainspec")?;

    for address in NetworkWallet::<Ethereum>::signer_addresses(wallet) {
        let substrate_address = eth_to_polkadot_address(&address);
        balances.push(json!((substrate_address, INITIAL_BALANCE)));
    }

    Ok(())
}
