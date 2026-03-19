use crate::internal_prelude::*;

/// Checks if the eth-rpc binary supports the `--cache-size` CLI argument
/// by inspecting its `--help` output. This is used for backwards compatibility
/// with older eth-rpc versions that require `--cache-size`.
pub fn eth_rpc_supports_cache_size(eth_rpc_binary: &Path) -> bool {
    Command::new(eth_rpc_binary)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|output| {
            let help_text = String::from_utf8_lossy(&output.stdout);
            help_text.contains("--cache-size")
        })
        .unwrap_or(false)
}
