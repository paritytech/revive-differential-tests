use crate::internal_prelude::*;

static CACHE_SIZE_SUPPORT: StdMutex<BTreeMap<PathBuf, bool>> = StdMutex::new(BTreeMap::new());

/// Appends `--cache-size <NUMBER_OF_CACHED_BLOCKS>` to `command` when the
/// given eth-rpc binary supports that flag. Used for backwards compatibility
/// with older eth-rpc versions that accept `--cache-size`.
pub fn apply_cache_size_arg(command: &mut Command, eth_rpc_binary: &Path) {
    if eth_rpc_supports_cache_size(eth_rpc_binary) {
        command
            .arg("--cache-size")
            .arg(NUMBER_OF_CACHED_BLOCKS.to_string());
    }
}

fn eth_rpc_supports_cache_size(eth_rpc_binary: &Path) -> bool {
    if let Some(&cached) = CACHE_SIZE_SUPPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(eth_rpc_binary)
    {
        return cached;
    }

    let supports = Command::new(eth_rpc_binary)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|output| {
            let help_text = String::from_utf8_lossy(&output.stdout);
            help_text.contains("--cache-size")
        })
        .unwrap_or(false);

    CACHE_SIZE_SUPPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(eth_rpc_binary.to_path_buf(), supports);
    supports
}
