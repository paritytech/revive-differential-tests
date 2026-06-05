# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Revive Differential Tests is a differential testing framework for Ethereum-compatible smart contract stacks. It compiles and executes declarative smart-contract tests against multiple platforms (Geth, Revive Dev Node, Zombienet, Polkadot Omni Node) and compares their behavior (status, return data, events, state diffs).

The test corpus lives in a separate repository: [resolc-compiler-tests](https://github.com/paritytech/resolc-compiler-tests) and is included as a git submodule.

## CLI Usage

Run `retester --help` and `report-processor --help` for up-to-date CLI usage information.

## Troubleshooting

### Important Resolc Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--resolc.heap-size` | 131072 | Emulated EVM heap memory buffer size in bytes. Increase for tests accessing large memory offsets. |
| `--resolc.stack-size` | 131072 | Contract stack size in bytes. |
| `--resolc.path` | (from PATH) | Path to resolc binary. |

These are **compile-time** settings embedded in the PVM bytecode. They are folded into the compilation cache fingerprint (along with a sha256 of the resolc and solc binaries), so changing them automatically invalidates affected cache entries — no manual cache clear required.

### Compilation Cache

Compilation artifacts are cached in the working directory. The cache key includes a sha256 fingerprint of the resolc and solc binaries plus the PVM heap/stack sizes, so swapping binaries or tweaking those settings invalidates the cache automatically. If you ever need to force a full rebuild, pass `--compilation.invalidate-cache` or delete the working directory:

```bash
rm -rf ./workdir
```

### Common Issues

1. **"Failed to find information for contract"** - The resolc output selection doesn't include bytecode. Ensure `crates/compiler/src/revive_resolc.rs` uses `new_required_for_tests()` instead of `new_required()`.

2. **"OutOfGas" errors on tests with large memory offsets** - Increase `--resolc.heap-size` (e.g., to 528000).

3. **"ContractTrapped" on memory access** - The contract is accessing memory beyond the configured heap size. Increase `--resolc.heap-size`.

4. **Gas estimation failures for reverting transactions** - The test may need `gas_overrides` in the test metadata to specify a hardcoded gas limit.
