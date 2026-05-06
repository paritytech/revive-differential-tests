//! Core assignment storage injection for elastic scaling.
//!
//! Generates raw storage entries for the `ParaScheduler` pallet,
//! pre-assigning cores to a parachain at genesis. This enables
//! elastic scaling (multiple cores per parachain) without requiring
//! the Coretime chain or governance actions.
//!
//! The scheduler pallet stores core assignments in two items:
//!
//! - `CoreSchedules`: A `StorageMap<Twox64Concat, (BlockNumber,
//!   CoreIndex), Schedule>` mapping `(block_number, core_index)` to
//!   a schedule.
//!
//! - `CoreDescriptors`: A `StorageValue<BTreeMap<CoreIndex,
//!   CoreDescriptor>>` holding descriptors for all cores.
//!
//! Because the zombienet SDK generates session keys during
//! `spawn_native()` and bakes them into the raw chainspec, we cannot
//! replace the chainspec directly. Instead we wrap the relay chain
//! binary so that `build-spec --raw` output is post-processed to
//! include our entries.
//!
//! The wrapper's Python script merges our entries into the raw
//! chainspec. For `CoreDescriptors` (a `StorageValue`), we must
//! merge our cores into the existing `BTreeMap` rather than
//! overwriting it, since `assign_coretime` already placed an entry
//! during genesis build.

use anyhow::Context as _;
use sp_core::hashing::{twox_64, twox_128};
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
///     assignments: vec![(
///         CoreAssignment::Task(para_id),
///         PartsOf57600(57600),
///     )],
///     end_hint: None,
///     next_schedule: None,
/// }
/// ```
fn encode_core_schedule(para_id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    // Vec<(CoreAssignment, PartsOf57600)> length 1,
    // compact-encoded.
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
/// The `on_initialize` hook at block 1 reads the queue, finds the
/// schedule at block 0, and populates `current_work`.
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

/// Full storage key for
/// `ParaScheduler::CoreSchedules[(block_number, core_index)]`.
///
/// Layout: `Twox128("ParaScheduler")
///       ++ Twox128("CoreSchedules")
///       ++ Twox64Concat(encode((block_number, core_index)))`
fn core_schedule_storage_key(block_number: u32, core_index: u32) -> String {
    let pallet_hash = twox_128(b"ParaScheduler");
    let storage_hash = twox_128(b"CoreSchedules");

    let mut map_key = [0u8; 8];
    map_key[..4].copy_from_slice(&block_number.to_le_bytes());
    map_key[4..].copy_from_slice(&core_index.to_le_bytes());

    // Twox64Concat: 8-byte hash ++ raw key
    let key_hash = twox_64(&map_key);

    let mut full_key = Vec::with_capacity(16 + 16 + 8 + map_key.len());
    full_key.extend_from_slice(&pallet_hash);
    full_key.extend_from_slice(&storage_hash);
    full_key.extend_from_slice(&key_hash);
    full_key.extend_from_slice(&map_key);
    to_hex_string(&full_key)
}

/// Full storage key for
/// `ParaScheduler::CoreDescriptors` (`StorageValue`).
///
/// Layout: `Twox128("ParaScheduler")
///       ++ Twox128("CoreDescriptors")`
fn core_descriptors_storage_key() -> String {
    let pallet_hash = twox_128(b"ParaScheduler");
    let storage_hash = twox_128(b"CoreDescriptors");

    let mut full_key = Vec::with_capacity(32);
    full_key.extend_from_slice(&pallet_hash);
    full_key.extend_from_slice(&storage_hash);
    to_hex_string(&full_key)
}

/// SCALE compact-encode a `u32` value.
#[cfg(test)]
fn scale_compact_u32(value: u32) -> Vec<u8> {
    if value < 64 {
        vec![(value as u8) << 2]
    } else if value < 16384 {
        let v = ((value as u16) << 2) | 1;
        v.to_le_bytes().to_vec()
    } else if value < 1_073_741_824 {
        let v = (value << 2) | 2;
        v.to_le_bytes().to_vec()
    } else {
        panic!("Compact encoding of values >= 2^30 not supported");
    }
}

/// SCALE-encode a `BTreeMap<CoreIndex, CoreDescriptor>` containing
/// entries for `core_indices`.
///
/// The map is encoded as a SCALE compact length prefix followed by
/// sorted `(CoreIndex, CoreDescriptor)` entries.
#[cfg(test)]
fn encode_core_descriptors_btree_map(core_indices: &[u32]) -> Vec<u8> {
    let descriptor = encode_core_descriptor();
    let len = core_indices.len() as u32;
    let len_bytes = scale_compact_u32(len);

    let mut buf = Vec::with_capacity(len_bytes.len() + core_indices.len() * (4 + descriptor.len()));
    buf.extend_from_slice(&len_bytes);
    for &core_idx in core_indices {
        buf.extend_from_slice(&core_idx.to_le_bytes());
        buf.extend_from_slice(&descriptor);
    }
    buf
}

/// Generate all raw storage entries to assign `num_cores` cores to
/// `para_id`.
///
/// Returns:
/// - `schedules`: A JSON map of hex storage keys to hex values for
///   `CoreSchedules` entries (one per core). These can be merged
///   directly into `genesis.raw.top`.
/// - `descriptors_key`: The hex storage key for the
///   `CoreDescriptors` `StorageValue`.
/// - `core_indices`: The core indices that need to be added to the
///   `CoreDescriptors` `BTreeMap`.
///
/// The `CoreDescriptors` value cannot be merged as a simple
/// key-value insertion because it is a `StorageValue<BTreeMap>`.
/// The caller must read the existing value, decode the `BTreeMap`,
/// insert the new entries, re-encode, and write it back. This is
/// handled by the wrapper's Python script.
pub fn generate_core_assignment_entries(num_cores: u32, para_id: u32) -> CoreAssignmentEntries {
    let schedule_value = to_hex_string(&encode_core_schedule(para_id));

    let mut schedule_entries = serde_json::Map::with_capacity(num_cores as usize);
    let mut core_indices = Vec::with_capacity(num_cores as usize);

    for core_idx in 0..num_cores {
        schedule_entries.insert(
            core_schedule_storage_key(0, core_idx),
            serde_json::Value::String(schedule_value.clone()),
        );
        core_indices.push(core_idx);
    }

    CoreAssignmentEntries {
        schedule_entries,
        descriptors_key: core_descriptors_storage_key(),
        core_indices,
        descriptor_bytes: to_hex_string(&encode_core_descriptor()),
    }
}

/// Data needed to inject core assignments into a raw chain spec.
pub struct CoreAssignmentEntries {
    /// `CoreSchedules` entries keyed by their full storage key.
    pub schedule_entries: serde_json::Map<String, serde_json::Value>,
    /// The storage key for the `CoreDescriptors` `StorageValue`.
    pub descriptors_key: String,
    /// Core indices to inject into the `CoreDescriptors` BTreeMap.
    pub core_indices: Vec<u32>,
    /// Hex-encoded SCALE bytes for a single `CoreDescriptor`.
    pub descriptor_bytes: String,
}

/// Generate a shell wrapper script that intercepts `--raw` calls
/// and injects core assignment storage entries into the output.
/// For all other invocations the wrapper passes through to the
/// real binary.
///
/// A bash script is used here because the zombienet SDK invokes
/// the `chain_spec_command` as a subprocess and expects it to
/// behave exactly like the original binary. The SDK's
/// `customize_relay()` step generates session keys and parachain
/// registrations, then calls `chain_spec_command ... --raw` to
/// convert the plain chainspec to raw. We cannot skip this step
/// (session keys would be missing), nor can we supply a pre-built
/// raw chainspec via `chain_spec_path` (the SDK skips
/// `customize_relay()` entirely in that case). A transparent
/// wrapper script is the simplest way to intercept only the
/// `--raw` conversion, merge our storage entries, and pass
/// everything else through unchanged.
///
/// The Python script in the wrapper performs two operations:
///
/// 1. Inserts `CoreSchedules` entries directly (they are a
///    `StorageMap`, so each core has its own key).
///
/// 2. Merges new cores into the existing `CoreDescriptors`
///    `StorageValue`. This is a `BTreeMap<CoreIndex,
///    CoreDescriptor>` so we must decode the existing value,
///    insert new entries, and re-encode.
pub fn generate_wrapper_script(
    real_binary_path: &str,
    schedules_file_path: &str,
    descriptors_key: &str,
    core_indices: &[u32],
    descriptor_hex: &str,
) -> String {
    let core_indices_str = core_indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");

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
import json, sys, struct

spec = json.load(open(sys.argv[1]))
schedules = json.load(open(sys.argv[2]))
descriptors_key = sys.argv[3]
core_indices = [int(x) for x in sys.argv[4].split(',')]
descriptor_hex = sys.argv[5]

top = spec.setdefault('genesis', {{}}).setdefault('raw', {{}}).setdefault('top', {{}})

# 1. Insert CoreSchedules entries (StorageMap - one key per core).
top.update(schedules)

# 2. Merge new cores into CoreDescriptors (StorageValue<BTreeMap>).
descriptor_bytes = bytes.fromhex(descriptor_hex[2:])

existing_hex = top.get(descriptors_key, '0x00')
existing_bytes = bytes.fromhex(existing_hex[2:])

# Decode existing BTreeMap<CoreIndex(u32), CoreDescriptor>.
entries = {{}}
if len(existing_bytes) > 0:
    pos = 0
    # SCALE compact length
    first_byte = existing_bytes[pos]
    if first_byte & 0x03 == 0:
        length = first_byte >> 2
        pos += 1
    elif first_byte & 0x03 == 1:
        length = int.from_bytes(existing_bytes[pos:pos+2], 'little') >> 2
        pos += 2
    elif first_byte & 0x03 == 2:
        length = int.from_bytes(existing_bytes[pos:pos+4], 'little') >> 2
        pos += 4
    else:
        raise ValueError('BigInt compact not supported')

    desc_len = len(descriptor_bytes)
    for _ in range(length):
        core_idx = int.from_bytes(existing_bytes[pos:pos+4], 'little')
        pos += 4
        desc = existing_bytes[pos:pos+desc_len]
        pos += desc_len
        entries[core_idx] = desc

# Add our new cores.
for ci in core_indices:
    entries[ci] = descriptor_bytes

# Re-encode the BTreeMap sorted by key.
sorted_keys = sorted(entries.keys())
count = len(sorted_keys)
if count < 64:
    length_enc = bytes([count << 2])
elif count < 16384:
    val = (count << 2) | 1
    length_enc = val.to_bytes(2, 'little')
else:
    val = (count << 2) | 2
    length_enc = val.to_bytes(4, 'little')

result = bytearray(length_enc)
for k in sorted_keys:
    result += k.to_bytes(4, 'little')
    result += entries[k]

top[descriptors_key] = '0x' + result.hex()

json.dump(spec, sys.stdout)
" "$tmpfile" "{schedules_file}" "{descriptors_key}" "{core_indices}" "{descriptor_hex}"
    exit 0
fi

exec "{real_binary}" "$@"
"#,
        real_binary = real_binary_path,
        schedules_file = schedules_file_path,
        descriptors_key = descriptors_key,
        core_indices = core_indices_str,
        descriptor_hex = descriptor_hex,
    )
}

/// Wraps the relay chain binary so that `build-spec --raw` output
/// includes core assignment storage entries, enabling elastic
/// scaling.
///
/// The zombienet SDK generates session keys during
/// `spawn_native()` and bakes them into the raw chainspec. We
/// cannot replace the chainspec directly, so instead we wrap the
/// relay chain's `chain_spec_command` binary with a script that
/// post-processes `--raw` output to inject our entries.
///
/// Skips (with debug logging) if the relay chain has no
/// `chain_spec_command`, no `num_cores` configured, or no
/// `relaychain` section.
pub fn inject_core_assignments(
    toml_value: &mut toml::Value,
    output_directory: &Path,
) -> anyhow::Result<()> {
    let relaychain = match toml_value.get("relaychain") {
        Some(r) => r,
        None => {
            tracing::debug!(
                "No relaychain section found; \
                 skipping core assignment injection"
            );
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
                "No num_cores configured (or zero); \
                 skipping core assignment injection"
            );
            return Ok(());
        }
    };

    // The relay chain scheduler automatically assigns one core to
    // a parachain when it is registered by zombienet's
    // customize_relay() step. We subtract that core so the total
    // (scheduler-assigned + injected) equals num_cores.
    let num_cores = num_cores.saturating_sub(1);
    if num_cores == 0 {
        tracing::debug!(
            "Only 1 core configured; the scheduler's \
             automatic assignment is sufficient"
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

    // Extract the chain_spec_command template and find the binary
    // name. Template format: "chain-spec-generator {{chainName}}"
    let chain_spec_cmd = match relaychain
        .get("chain_spec_command")
        .and_then(|v| v.as_str())
    {
        Some(cmd) => cmd.to_string(),
        None => {
            tracing::debug!(
                "No chain_spec_command in relaychain; \
                 skipping core assignment injection"
            );
            return Ok(());
        }
    };

    // The binary is the first token of the command template.
    let spec_binary = chain_spec_cmd
        .split_whitespace()
        .next()
        .context("chain_spec_command is empty")?;

    // Resolve the absolute path of the chain-spec-generator
    // binary.
    let which_output = Command::new("which")
        .arg(spec_binary)
        .output()
        .context("Failed to locate chain spec generator binary")?;
    let real_binary_path = String::from_utf8_lossy(&which_output.stdout)
        .trim()
        .to_string();
    if real_binary_path.is_empty() {
        anyhow::bail!(
            "Chain spec generator binary \
             '{spec_binary}' not found in PATH"
        );
    }

    let entries = generate_core_assignment_entries(num_cores, para_id);

    // 1. Write the CoreSchedules entries as a JSON file.
    let schedules_path = output_directory.join("core_assignment_entries.json");
    std::fs::write(
        &schedules_path,
        serde_json::to_string(&entries.schedule_entries).context("Failed to serialize entries")?,
    )
    .context("Failed to write entries file")?;

    // 2. Generate the wrapper script.
    let wrapper_path = output_directory.join("chain-spec-generator-wrapper.sh");
    let script = generate_wrapper_script(
        &real_binary_path,
        &schedules_path.to_string_lossy(),
        &entries.descriptors_key,
        &entries.core_indices,
        &entries.descriptor_bytes,
    );

    std::fs::write(&wrapper_path, script).context("Failed to write wrapper script")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))
            .context("Failed to make wrapper script executable")?;
    }

    // 3. Replace chain_spec_command with the wrapper, preserving
    //    the template arguments (e.g. "{{chainName}}").
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
        num_entries = entries.schedule_entries.len(),
        wrapper = %wrapper_path.display(),
        "Relay chain chain_spec_command wrapped for \
         core assignment injection"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_schedule_encoding() {
        // Arrange
        let para_id = 1000u32;

        // Act
        let encoded = encode_core_schedule(para_id);

        // Assert
        assert_eq!(
            encoded,
            vec![0x04, 0x02, 0xe8, 0x03, 0x00, 0x00, 0x00, 0xe1, 0x00, 0x00]
        );
    }

    #[test]
    fn core_descriptor_encoding() {
        // Arrange
        // (no setup needed)

        // Act
        let encoded = encode_core_descriptor();

        // Assert
        assert_eq!(
            encoded,
            vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn generates_correct_number_of_schedule_entries() {
        // Arrange
        let num_cores = 3;
        let para_id = 1000;

        // Act
        let entries = generate_core_assignment_entries(num_cores, para_id);

        // Assert
        assert_eq!(entries.schedule_entries.len(), 3);
        assert_eq!(entries.core_indices, vec![0, 1, 2]);
        for (key, value) in &entries.schedule_entries {
            assert!(key.starts_with("0x"));
            assert!(value.as_str().unwrap().starts_with("0x"));
        }
    }

    #[test]
    fn storage_keys_use_para_scheduler_prefix() {
        // Arrange
        let pallet_prefix = to_hex_string(&twox_128(b"ParaScheduler"));

        // Act
        let schedule_key = core_schedule_storage_key(0, 0);
        let descriptor_key = core_descriptors_storage_key();

        // Assert
        assert!(
            schedule_key.starts_with(&pallet_prefix),
            "CoreSchedules key must use ParaScheduler prefix"
        );
        assert!(
            descriptor_key.starts_with(&pallet_prefix),
            "CoreDescriptors key must use ParaScheduler prefix"
        );
    }

    #[test]
    fn schedule_keys_use_twox64_concat_hasher() {
        // Arrange
        let map_key_bytes = [0u8; 8]; // (block=0, core=0)
        let hash = twox_64(&map_key_bytes);

        // Act
        let key = core_schedule_storage_key(0, 0);
        let key_bytes = decode_hex(&key[2..]);

        // Assert
        // After the 32-byte prefix (pallet + storage), the next
        // 8 bytes should be the twox_64 hash, followed by the
        // raw key bytes.
        assert_eq!(&key_bytes[32..40], &hash);
        assert_eq!(&key_bytes[40..48], &map_key_bytes);
    }

    #[test]
    fn descriptor_key_is_storage_value() {
        // Arrange
        // StorageValue key = pallet hash + storage hash = 32 bytes

        // Act
        let key = core_descriptors_storage_key();
        let key_bytes = decode_hex(&key[2..]);

        // Assert
        assert_eq!(
            key_bytes.len(),
            32,
            "StorageValue key should be exactly 32 bytes \
             (pallet + storage hashes)"
        );
    }

    /// Decode a hex string into bytes (test helper).
    fn decode_hex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn different_cores_produce_different_schedule_keys() {
        // Arrange
        // (no setup needed)

        // Act
        let key0 = core_schedule_storage_key(0, 0);
        let key1 = core_schedule_storage_key(0, 1);
        let key2 = core_schedule_storage_key(0, 2);

        // Assert
        assert_ne!(key0, key1);
        assert_ne!(key1, key2);
        assert_ne!(key0, key2);
    }

    #[test]
    fn wrapper_script_contains_binary_path() {
        // Arrange
        let binary = "/usr/bin/polkadot";
        let entries = "/tmp/entries.json";
        let desc_key = "0xabcd";
        let cores = &[0u32, 1];
        let desc_hex = "0x1234";

        // Act
        let script = generate_wrapper_script(binary, entries, desc_key, cores, desc_hex);

        // Assert
        assert!(script.contains(binary));
        assert!(script.contains(entries));
        assert!(script.starts_with("#!/usr/bin/env bash"));
        assert!(script.contains(desc_key));
        assert!(script.contains("0,1"));
    }

    #[test]
    fn btree_map_encoding_matches_scale() {
        // Arrange
        let cores = [0u32, 1];
        let descriptor = encode_core_descriptor();

        // Act
        let encoded = encode_core_descriptors_btree_map(&cores);

        // Assert
        // compact(2) = 0x08
        assert_eq!(encoded[0], 0x08);
        // CoreIndex(0) = [0,0,0,0]
        assert_eq!(&encoded[1..5], &0u32.to_le_bytes());
        // descriptor for core 0
        assert_eq!(&encoded[5..5 + descriptor.len()], &descriptor);
        // CoreIndex(1) = [1,0,0,0]
        let offset = 5 + descriptor.len();
        assert_eq!(&encoded[offset..offset + 4], &1u32.to_le_bytes());
        // descriptor for core 1
        assert_eq!(
            &encoded[offset + 4..offset + 4 + descriptor.len()],
            &descriptor
        );
    }
}
