<div align="center">
  <h1><code>Revive Differential Tests</code></h1>

  <p>
    <strong>Differential testing for Ethereum-compatible smart contract stacks</strong>
  </p>
</div>

This project compiles and executes declarative smart-contract tests against multiple platforms, then compares behavior (status, return data, events, and state diffs). Today it supports:

- Geth (EVM reference implementation)
- Revive Kitchensink (Substrate-based PolkaVM + `eth-rpc` proxy)

Use it to:

- Detect observable differences between platforms (execution success, logs, state changes)
- Ensure reproducible builds across compilers/hosts
- Run end-to-end regression suites

This framework uses the [MatterLabs tests format](https://github.com/matter-labs/era-compiler-tests/tree/main/solidity) for declarative tests which is composed of the following:

- Metadata files, this is akin to a module of tests in Rust.
- Each metadata file contains multiple cases, a case is akin to a Rust test where a module can contain multiple tests.
- Each case contains multiple steps and assertions, this is akin to any Rust test that contains multiple statements.

Metadata files are JSON files, but Solidity files can also be metadata files if they include inline metadata provided as a comment at the top of the contract.

All of the steps contained within each test case are either:

- Transactions that need to be submitted and assertions to run on the submitted transactions.
- Assertions on the state of the chain (e.g., account balances, storage, etc...)

All of the transactions submitted by the this tool to the test nodes follow a similar logic to what wallets do. We first use alloy to estimate the transaction fees, then we attach that to the transaction and submit it to the node and then await the transaction receipt.

This repository contains none of the tests and only contains the testing framework or the test runner. The tests can be found in the [`resolc-compiler-tests`](https://github.com/paritytech/resolc-compiler-tests) repository which is a clone of [MatterLab's test suite](https://github.com/matter-labs/era-compiler-tests) with some modifications and adjustments made to suit our use case.

## Requirements

This section describes the required dependencies that this framework requires to run. Compiling this framework is pretty straightforward and no additional dependencies beyond what's specified in the `Cargo.toml` file should be required.

- Stable Rust
- Geth - When doing differential testing against the PVM we submit transactions to a Geth node and to Kitchensink to compare them.
- Kitchensink - When doing differential testing against the PVM we submit transactions to a Geth node and to Kitchensink to compare them.
- ETH-RPC - All communication with Kitchensink is done through the ETH RPC.
- Solc - This is actually a transitive dependency, while this tool doesn't require solc as it downloads the versions that it requires, resolc requires that Solc is installed and available in the path.
- Resolc - This is required to compile the contracts to PolkaVM bytecode.
- Kurtosis - The Kurtosis CLI tool is required for the production Ethereum mainnet-like node configuration with Geth as the execution layer and lighthouse as the consensus layer. Kurtosis also requires docker to be installed since it runs everything inside of docker containers.

All of the above need to be installed and available in the path in order for the tool to work.

## Running The Tool

This tool is being updated quite frequently. Therefore, it's recommended that you don't install the tool and then run it, but rather that you run it from the root of the directory using `cargo run --release`. The help command of the tool gives you all of the information you need to know about each of the options and flags that the tool offers.

> [!NOTE]  
> Note that the tests can be found in the [`resolc-compiler-tests`](https://github.com/paritytech/resolc-compiler-tests) repository.

The simplest command to run this tool is the following:

```bash
RUST_LOG="info" cargo run --release -- test \
    --test ./resolc-compiler-tests/fixtures/solidity \
    --platform geth-evm-solc \
    --working-directory workdir \
    > logs.log \
    2> output.log
```

The above command will run the tool executing every one of the tests discovered in the path provided to the tool. All of the logs from the execution will be persisted in the `logs.log` file and all of the output of the tool will be persisted to the `output.log` file. If all that you're looking for is to run the tool and check which tests succeeded and failed, then the `output.log` file is what you need to be looking at. However, if you're contributing the to the tool then the `logs.log` file will be very valuable.

<details>
<summary>User Managed Nodes</summary>

This section describes how the user can make use of nodes that they manage rather than allowing the tool to spawn and manage the nodes on the user's behalf.

> ⚠️ This is an advanced feature of the tool and could lead test successes or failures to not be reproducible. Please use this feature with caution and only if you understand the implications of running your own node instead of having the framework manage your nodes. ⚠️

If you're an advanced user and you'd like to manage your own nodes instead of having the tool initialize, spawn, and manage them, then you can choose to run your own nodes and then provide them to the tool to make use of just like the following:

```bash
#!/usr/bin/env bash
set -euo pipefail

PLATFORM="revive-dev-node-revm-solc"

retester export-genesis "$PLATFORM" > chainspec.json

# Start revive-dev-node in a detached tmux session
tmux new-session -d -s revive-dev-node \
  'RUST_LOG="error,evm=debug,sc_rpc_server=info,runtime::revive=debug" revive-dev-node \
    --dev \
    --chain chainspec.json \
    --force-authoring \
    --rpc-methods Unsafe \
    --rpc-cors all \
    --rpc-max-connections 4294967295 \
    --pool-limit 4294967295 \
    --pool-kbytes 4294967295'
sleep 5

# Start eth-rpc in a detached tmux session
tmux new-session -d -s eth-rpc \
  'RUST_LOG="info,eth-rpc=debug" eth-rpc \
    --dev \
    --node-rpc-url ws://127.0.0.1:9944 \
    --rpc-max-connections 4294967295'
sleep 5

# Run the tests (logs to files as before)
RUST_LOG="info" retester test \
  --platform "$PLATFORM" \
  --corpus ./revive-differential-tests/fixtures/solidity \
  --working-directory ./workdir \
  --concurrency.number-of-nodes 1 \
  --concurrency.number-of-concurrent-tasks 5 \
  --revive-dev-node.existing-rpc-url "http://localhost:8545" \
  > logs.log
```

</details>
