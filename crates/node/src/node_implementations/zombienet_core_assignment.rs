//! Core assignment storage injection for elastic scaling.
//!
//! Generates raw storage entries for the `CoretimeAssignmentProvider` pallet,
//! pre-assigning cores to a parachain at genesis. This enables elastic scaling
//! (multiple cores per parachain) without requiring the Coretime chain or
//! governance actions.
//!
//! The pallet has no `#[pallet::genesis_config]`, so we inject SCALE-encoded
//! storage entries directly into the raw chainspec's `genesis.raw.top`.
//!
//! Because the zombienet SDK generates session keys during `spawn_native()` and
//! bakes them into the raw chainspec, we cannot replace the chainspec directly.
//! Instead we wrap the relay chain binary so that `build-spec --raw` output is
//! post-processed to include our entries.

use anyhow::Context as _;
use sp_core::hashing::{twox_128, twox_256};
use std::fmt::Write;
use std::path::Path;
use std::process::Command;

/// Convert bytes to a `0x`-prefixed hex string.
fn to_hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

/// SCALE-encode a `Schedule<BlockNumber>` value:
///
/// ```ignore
/// Schedule {
///     assignments: vec![(CoreAssignment::Task(para_id), PartsOf57600(57600))],
///     end_hint: None,
///     next_schedule: None,
/// }
/// ```
fn encode_core_schedule(para_id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    // Vec<(CoreAssignment, PartsOf57600)> length 1, compact-encoded
    buf.push(0x04); // compact(1) = 1 << 2
    // CoreAssignment::Task(para_id) — enum variant index 2
    buf.push(0x02);
    buf.extend_from_slice(&para_id.to_le_bytes());
    // PartsOf57600(57600) — full core allocation
    buf.extend_from_slice(&57600u16.to_le_bytes());
    // end_hint: Option<BlockNumber> = None
    buf.push(0x00);
    // next_schedule: Option<BlockNumber> = None
    buf.push(0x00);
    buf
}

/// SCALE-encode a `CoreDescriptor<BlockNumber>` value:
///
/// ```ignore
/// CoreDescriptor {
///     queue: Some(QueueDescriptor { first: 0, last: 0 }),
///     current_work: None,
/// }
/// ```
///
/// The `on_initialize` hook at block 1 reads the queue, finds the schedule
/// at block 0, and populates `current_work`.
fn encode_core_descriptor() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10);
    // queue: Option<QueueDescriptor> = Some
    buf.push(0x01);
    // QueueDescriptor { first: 0u32, last: 0u32 }
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    // current_work: Option<WorkState> = None
    buf.push(0x00);
    buf
}

/// Full storage key for `CoreSchedules[(block_number, core_index)]`.
///
/// Layout: `Twox128("CoretimeAssignmentProvider") ++ Twox128("CoreSchedules") ++ Twox256(encode((block_number, core_index)))`
fn core_schedule_storage_key(block_number: u32, core_index: u32) -> String {
    let pallet_hash = twox_128(b"CoretimeAssignmentProvider");
    let storage_hash = twox_128(b"CoreSchedules");

    let mut map_key = [0u8; 8];
    map_key[..4].copy_from_slice(&block_number.to_le_bytes());
    map_key[4..].copy_from_slice(&core_index.to_le_bytes());
    let key_hash = twox_256(&map_key);

    let mut full_key = Vec::with_capacity(64);
    full_key.extend_from_slice(&pallet_hash);
    full_key.extend_from_slice(&storage_hash);
    full_key.extend_from_slice(&key_hash);
    to_hex_string(&full_key)
}

/// Full storage key for `CoreDescriptors[core_index]`.
///
/// Layout: `Twox128("CoretimeAssignmentProvider") ++ Twox128("CoreDescriptors") ++ Twox256(encode(core_index))`
fn core_descriptor_storage_key(core_index: u32) -> String {
    let pallet_hash = twox_128(b"CoretimeAssignmentProvider");
    let storage_hash = twox_128(b"CoreDescriptors");

    let key_hash = twox_256(&core_index.to_le_bytes());

    let mut full_key = Vec::with_capacity(64);
    full_key.extend_from_slice(&pallet_hash);
    full_key.extend_from_slice(&storage_hash);
    full_key.extend_from_slice(&key_hash);
    to_hex_string(&full_key)
}

/// Generate all raw storage entries to assign `num_cores` cores to `para_id`.
///
/// Returns a JSON map of hex storage keys to hex values, suitable for merging
/// into `genesis.raw.top`.
pub fn generate_core_assignment_entries_json(
    num_cores: u32,
    para_id: u32,
) -> serde_json::Map<String, serde_json::Value> {
    let schedule_value = to_hex_string(&encode_core_schedule(para_id));
    let descriptor_value = to_hex_string(&encode_core_descriptor());

    let mut map = serde_json::Map::with_capacity(num_cores as usize * 2);
    for core_idx in 0..num_cores {
        map.insert(
            core_schedule_storage_key(0, core_idx),
            serde_json::Value::String(schedule_value.clone()),
        );
        map.insert(
            core_descriptor_storage_key(core_idx),
            serde_json::Value::String(descriptor_value.clone()),
        );
    }
    map
}

/// Generate a shell wrapper script that intercepts `--raw` calls
/// and injects core assignment storage entries into the output.
/// For all other invocations the wrapper passes through to the real binary.
///
/// A bash script is used here because the zombienet SDK invokes the
/// `chain_spec_command` as a subprocess and expects it to behave exactly
/// like the original binary. The SDK's `customize_relay()` step generates
/// session keys and parachain registrations, then calls
/// `chain_spec_command ... --raw` to convert the plain chainspec to raw.
/// We cannot skip this step (session keys would be missing), nor can we
/// supply a pre-built raw chainspec via `chain_spec_path` (the SDK skips
/// `customize_relay()` entirely in that case). A transparent wrapper
/// script is the simplest way to intercept only the `--raw` conversion,
/// merge our storage entries, and pass everything else through unchanged.
pub fn generate_wrapper_script(real_binary_path: &str, entries_file_path: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

has_raw=false
for arg in "$@"; do
    case "$arg" in
        --raw) has_raw=true ;;
    esac
done

if $has_raw; then
    tmpfile=$(mktemp)
    trap 'rm -f "$tmpfile"' EXIT
    "{real_binary}" "$@" > "$tmpfile"
    python3 -c "
import json, sys
spec = json.load(open(sys.argv[1]))
entries = json.load(open(sys.argv[2]))
spec.setdefault('genesis', {{}}).setdefault('raw', {{}}).setdefault('top', {{}}).update(entries)
json.dump(spec, sys.stdout)
" "$tmpfile" "{entries_file}"
    exit 0
fi

exec "{real_binary}" "$@"
"#,
        real_binary = real_binary_path,
        entries_file = entries_file_path,
    )
}

/// Wraps the relay chain binary so that `build-spec --raw` output includes
/// core assignment storage entries, enabling elastic scaling.
///
/// The zombienet SDK generates session keys during `spawn_native()` and
/// bakes them into the raw chainspec. We cannot replace the chainspec
/// directly, so instead we wrap the relay chain's `chain_spec_command`
/// binary with a script that post-processes `--raw` output to inject our
/// entries.
///
/// Skips (with debug logging) if the relay chain has no `chain_spec_command`,
/// no `num_cores` configured, or no `relaychain` section.
pub fn inject_core_assignments(
    toml_value: &mut toml::Value,
    output_directory: &Path,
) -> anyhow::Result<()> {
    let relaychain = match toml_value.get("relaychain") {
        Some(r) => r,
        None => {
            tracing::debug!("No relaychain section found; skipping core assignment injection");
            return Ok(());
        }
    };

    // Extract num_cores; skip if not configured.
    let num_cores = relaychain
        .get("genesis")
        .and_then(|g| g.get("runtimeGenesis"))
        .and_then(|rg| rg.get("patch"))
        .and_then(|p| p.get("configuration"))
        .and_then(|c| c.get("config"))
        .and_then(|c| c.get("scheduler_params"))
        .and_then(|sp| sp.get("num_cores"))
        .and_then(|n| n.as_integer());
    let num_cores = match num_cores {
        Some(n) if n > 0 => n as u32,
        _ => {
            tracing::debug!(
                "No num_cores configured (or zero); skipping core assignment injection"
            );
            return Ok(());
        }
    };

    // The relay chain scheduler automatically assigns one core to a parachain
    // when it is registered by zombienet's customize_relay() step. We subtract
    // that core so the total (scheduler-assigned + injected) equals num_cores.
    let num_cores = num_cores.saturating_sub(1);
    if num_cores == 0 {
        tracing::debug!(
            "Only 1 core configured; the scheduler's automatic assignment is sufficient"
        );
        return Ok(());
    }

    let para_id = toml_value
        .get("parachains")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
        .and_then(|p| p.get("id"))
        .and_then(|id| id.as_integer())
        .context("No parachain id found in zombienet config")? as u32;

    // Extract the chain_spec_command template and find the binary name.
    // Template format: "chain-spec-generator {{chainName}}"
    let chain_spec_cmd = match relaychain
        .get("chain_spec_command")
        .and_then(|v| v.as_str())
    {
        Some(cmd) => cmd.to_string(),
        None => {
            tracing::debug!(
                "No chain_spec_command in relaychain; skipping core assignment injection"
            );
            return Ok(());
        }
    };

    // The binary is the first token of the command template.
    let spec_binary = chain_spec_cmd
        .split_whitespace()
        .next()
        .context("chain_spec_command is empty")?;

    // Resolve the absolute path of the chain-spec-generator binary.
    let which_output = Command::new("which")
        .arg(spec_binary)
        .output()
        .context("Failed to locate chain spec generator binary")?;
    let real_binary_path = String::from_utf8_lossy(&which_output.stdout)
        .trim()
        .to_string();
    if real_binary_path.is_empty() {
        anyhow::bail!("Chain spec generator binary '{spec_binary}' not found in PATH");
    }

    // 1. Write the storage entries as a JSON file.
    let entries = generate_core_assignment_entries_json(num_cores, para_id);
    let entries_path = output_directory.join("core_assignment_entries.json");
    std::fs::write(
        &entries_path,
        serde_json::to_string(&entries).context("Failed to serialize entries")?,
    )
    .context("Failed to write entries file")?;

    // 2. Generate the wrapper script.
    let wrapper_path = output_directory.join("chain-spec-generator-wrapper.sh");
    let script = generate_wrapper_script(&real_binary_path, &entries_path.to_string_lossy());

    std::fs::write(&wrapper_path, script).context("Failed to write wrapper script")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))
            .context("Failed to make wrapper script executable")?;
    }

    // 3. Replace chain_spec_command with the wrapper, preserving the
    //    template arguments (e.g. "{{chainName}}").
    let wrapper_cmd = chain_spec_cmd.replacen(spec_binary, &wrapper_path.to_string_lossy(), 1);
    let relay_table = toml_value
        .get_mut("relaychain")
        .and_then(|v| v.as_table_mut())
        .context("relaychain is not a table")?;

    relay_table.insert(
        "chain_spec_command".to_string(),
        toml::Value::String(wrapper_cmd),
    );

    tracing::info!(
        num_cores,
        para_id,
        num_entries = entries.len(),
        wrapper = %wrapper_path.display(),
        "Relay chain chain_spec_command wrapped for core assignment injection"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_schedule_encoding() {
        let encoded = encode_core_schedule(1000);
        assert_eq!(
            encoded,
            vec![0x04, 0x02, 0xe8, 0x03, 0x00, 0x00, 0x00, 0xe1, 0x00, 0x00]
        );
    }

    #[test]
    fn core_descriptor_encoding() {
        let encoded = encode_core_descriptor();
        assert_eq!(
            encoded,
            vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn generates_correct_number_of_entries() {
        let entries = generate_core_assignment_entries_json(3, 1000);
        // 3 cores × 2 entries each (schedule + descriptor)
        assert_eq!(entries.len(), 6);
        for (key, value) in &entries {
            assert!(key.starts_with("0x"));
            assert!(value.as_str().unwrap().starts_with("0x"));
        }
    }

    #[test]
    fn storage_keys_have_correct_length() {
        let schedule_key = core_schedule_storage_key(0, 0);
        let descriptor_key = core_descriptor_storage_key(0);
        // 16 + 16 + 32 = 64 bytes = 128 hex chars + "0x" prefix
        assert_eq!(schedule_key.len(), 130);
        assert_eq!(descriptor_key.len(), 130);
    }

    #[test]
    fn different_cores_produce_different_keys() {
        let key0 = core_schedule_storage_key(0, 0);
        let key1 = core_schedule_storage_key(0, 1);
        let key2 = core_schedule_storage_key(0, 2);
        assert_ne!(key0, key1);
        assert_ne!(key1, key2);
        assert_ne!(key0, key2);
    }

    #[test]
    fn wrapper_script_contains_binary_path() {
        let script = generate_wrapper_script("/usr/bin/polkadot", "/tmp/entries.json");
        assert!(script.contains("/usr/bin/polkadot"));
        assert!(script.contains("/tmp/entries.json"));
        assert!(script.starts_with("#!/usr/bin/env bash"));
    }
}
