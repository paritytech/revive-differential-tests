# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Revive Differential Tests is a differential testing framework for Ethereum-compatible smart contract stacks. It compiles and executes declarative smart-contract tests against multiple platforms (Geth, Revive Dev Node, Zombienet, Polkadot Omni Node) and compares their behavior (status, return data, events, state diffs).

The test corpus lives in a separate repository: [resolc-compiler-tests](https://github.com/paritytech/resolc-compiler-tests) and is included as a git submodule.

## Build and Test Commands

```bash
# Build release binary
cargo build --release

# Install retester and report-processor binaries
cargo install --path crates/core
cargo install --path crates/report-processor

# Run unit tests
cargo make test

# Linting and formatting
cargo make fmt-check      # Check formatting
cargo make clippy         # Run clippy with --deny warnings
cargo make machete        # Check for unused dependencies
```

## Running Differential Tests

### Full Test Suite (PolkaVM with Resolc)

This is how the polkadot-sdk CI runs tests:

```bash
retester test \
    --test ./resolc-compiler-tests/fixtures/solidity/simple \
    --test ./resolc-compiler-tests/fixtures/solidity/complex \
    --test ./resolc-compiler-tests/fixtures/solidity/translated_semantic_tests \
    -p revive-dev-node-polkavm-resolc \
    --report.file-name report.json \
    --concurrency.number-of-nodes 10 \
    --concurrency.number-of-threads 10 \
    --concurrency.number-of-concurrent-tasks 100 \
    --working-directory ./workdir \
    --revive-dev-node.consensus manual-seal-200 \
    --resolc.heap-size 528000 \
    --resolc.stack-size 128000
```

### Important Resolc Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--resolc.heap-size` | 131072 | Emulated EVM heap memory buffer size in bytes. Increase for tests accessing large memory offsets. |
| `--resolc.stack-size` | 131072 | Contract stack size in bytes. |
| `--resolc.path` | (from PATH) | Path to resolc binary. |

**Critical:** These are **compile-time** settings embedded in the PVM bytecode. Changing them requires clearing the compilation cache (see Troubleshooting).

### Test Specifiers

```bash
# Run all tests in a directory
-t ./resolc-compiler-tests/fixtures/solidity/simple

# Run specific test file
-t path/to/test.sol

# Run specific case within a metadata file (case_idx is 0-indexed)
-t path/to/metadata.json::0

# Run specific case with specific mode
-t path/to/metadata.json::0::Y\ M0
```

### Report Processing

```bash
# Generate expectations file from test report
report-processor generate-expectations-file \
    --report-path ./workdir/report.json \
    --output-path expectations.json \
    --remove-prefix ./resolc-compiler-tests \
    --include-status failed
```

## Supported Platforms

| Platform CLI Name | Description |
|-------------------|-------------|
| `geth-evm-solc` | Go-Ethereum reference EVM + Solc |
| `lighthouse-geth-evm-solc` | Geth with Lighthouse consensus + Solc |
| `revive-dev-node-polkavm-resolc` | Substrate PolkaVM + Resolc |
| `revive-dev-node-revm-solc` | Substrate REVM + Solc |
| `zombienet-polkavm-resolc` | Zombienet Substrate + Resolc |
| `zombienet-revm-solc` | Zombienet REVM + Solc |
| `polkadot-omni-node-polkavm-resolc` | Polkadot Omni Node + Resolc |
| `polkadot-omni-node-revm-solc` | Polkadot Omni Node + Solc |

## Architecture

The project is a Rust workspace (edition 2024, MSRV 1.87.0) with crates in `/crates/`:

- **core** (`retester` binary) - Main test runner and orchestrator
- **config** - CLI argument parsing via Clap; defines `Context` enum (Test, Benchmark, ExportJsonSchema, ExportGenesis)
- **format** - Declarative test definition format based on MatterLabs; includes `gas_overrides` support for platform-specific gas limits
- **common** - Shared types including `PlatformIdentifier`, `ParsedTestSpecifier`, `Mode`, `ParsedMode`
- **compiler** - Compilation abstraction for Solc and Resolc compilers
  - `revive_resolc.rs`: Uses `SolcStandardJsonInputSettingsSelection::new_required_for_tests()` to request `evm.bytecode` output (required for resolc 0.6.0+)
- **node** - Platform/node implementations in `src/node_implementations/`
- **node-interaction** - Transaction submission, gas estimation via alloy, receipt waiting
  - `fallback_gas_filler.rs`: Handles gas estimation for reverting transactions using debug traces
- **report** - Test result aggregation and reporting (JSON format)
- **report-processor** - Secondary binary for post-execution report analysis
- **solc-binaries** - Solc compiler binary caching and downloads

## Test Format

Tests use the MatterLabs format with metadata either as JSON files or inline Solidity comments:

```
Corpus (directory)
└── Metadata (JSON file or Solidity with inline comments)
    └── Case (individual test)
        ├── targets (optional): filter to specific platforms
        ├── modes (optional): restrict to specific compiler modes (e.g., ["E-"] for EVM assembly only)
        ├── gas_overrides (optional): platform-specific hardcoded gas limits
        └── Steps
            ├── Transaction steps with assertions
            └── State assertions (balances, storage)
```

### Gas Overrides

For tests that fail gas estimation (e.g., due to `consume_all_gas` behavior in resolc), use `gas_overrides`:

```json
{
  "method": "test",
  "calldata": ["0x100000000"],
  "gas_overrides": {
    "revive-dev-node-polkavm-resolc": {
      "gas_limit": 10000000
    }
  }
}
```

## External Dependencies

**Always required:**
- Solc (Solidity compiler) - auto-downloaded but resolc needs it in PATH

**Platform-specific:**
| Platform | Required Tools |
|----------|----------------|
| `geth-evm-solc` | Geth |
| `lighthouse-geth-evm-solc` | Geth, Kurtosis, Docker |
| `revive-dev-node-*` | revive-dev-node, eth-rpc |
| `zombienet-*` | Zombienet SDK binaries |
| `polkadot-omni-node-*` | polkadot-omni-node, eth-rpc |

**For PolkaVM platforms:** Resolc compiler

## Troubleshooting

### Clearing the Compilation Cache

Compilation artifacts are cached in the working directory. If you change compiler settings (like `--resolc.heap-size`), you must clear the cache:

```bash
rm -rf ./workdir
```

### Common Issues

1. **"Failed to find information for contract"** - The resolc output selection doesn't include bytecode. Ensure `crates/compiler/src/revive_resolc.rs` uses `new_required_for_tests()` instead of `new_required()`.

2. **"OutOfGas" errors on tests with large memory offsets** - Increase `--resolc.heap-size` (e.g., to 528000) and clear the compilation cache.

3. **"ContractTrapped" on memory access** - The contract is accessing memory beyond the configured heap size. Increase `--resolc.heap-size`.

4. **Tests failing after changing resolc parameters** - Clear the compilation cache (`rm -rf ./workdir`) to force recompilation.

5. **Gas estimation failures for reverting transactions** - The test may need `gas_overrides` in the test metadata to specify a hardcoded gas limit.

## Updating the Test Submodule

```bash
git submodule update --init --remote resolc-compiler-tests
```
