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

```bash
$ cargo run --release -- execute-tests --help
Error: Executes tests in the MatterLabs format differentially on multiple targets concurrently

Usage: retester execute-tests [OPTIONS]

Options:
  -w, --working-directory <WORKING_DIRECTORY>
          The working directory that the program will use for all of the temporary artifacts needed at runtime.

          If not specified, then a temporary directory will be created and used by the program for all temporary artifacts.

          [default: ]

  -p, --platform <PLATFORMS>
          The set of platforms that the differential tests should run on

          [default: geth-evm-solc,revive-dev-node-polkavm-resolc]

          Possible values:
          - geth-evm-solc:                  The Go-ethereum reference full node EVM implementation with the solc compiler
          - kitchensink-polkavm-resolc:     The kitchensink node with the PolkaVM backend with the resolc compiler
          - kitchensink-revm-solc:          The kitchensink node with the REVM backend with the solc compiler
          - revive-dev-node-polkavm-resolc: The revive dev node with the PolkaVM backend with the resolc compiler
          - revive-dev-node-revm-solc:      The revive dev node with the REVM backend with the solc compiler

  -c, --corpus <CORPUS>
          A list of test corpus JSON files to be tested

  -h, --help
          Print help (see a summary with '-h')

Solc Configuration:
      --solc.version <VERSION>
          Specifies the default version of the Solc compiler that should be used if there is no override specified by one of the test cases

          [default: 0.8.29]

Resolc Configuration:
      --resolc.path <resolc.path>
          Specifies the path of the resolc compiler to be used by the tool.

          If this is not specified, then the tool assumes that it should use the resolc binary that's provided in the user's $PATH.

          [default: resolc]

Geth Configuration:
      --geth.path <geth.path>
          Specifies the path of the geth node to be used by the tool.

          If this is not specified, then the tool assumes that it should use the geth binary that's provided in the user's $PATH.

          [default: geth]

      --geth.start-timeout-ms <geth.start-timeout-ms>
          The amount of time to wait upon startup before considering that the node timed out

          [default: 5000]

Kitchensink Configuration:
      --kitchensink.path <kitchensink.path>
          Specifies the path of the kitchensink node to be used by the tool.

          If this is not specified, then the tool assumes that it should use the kitchensink binary that's provided in the user's $PATH.

          [default: substrate-node]

      --kitchensink.start-timeout-ms <kitchensink.start-timeout-ms>
          The amount of time to wait upon startup before considering that the node timed out

          [default: 5000]

      --kitchensink.dont-use-dev-node
          This configures the tool to use Kitchensink instead of using the revive-dev-node

Revive Dev Node Configuration:
      --revive-dev-node.path <revive-dev-node.path>
          Specifies the path of the revive dev node to be used by the tool.

          If this is not specified, then the tool assumes that it should use the revive dev node binary that's provided in the user's $PATH.

          [default: revive-dev-node]

      --revive-dev-node.start-timeout-ms <revive-dev-node.start-timeout-ms>
          The amount of time to wait upon startup before considering that the node timed out

          [default: 5000]

Eth RPC Configuration:
      --eth-rpc.path <eth-rpc.path>
          Specifies the path of the ETH RPC to be used by the tool.

          If this is not specified, then the tool assumes that it should use the ETH RPC binary that's provided in the user's $PATH.

          [default: eth-rpc]

      --eth-rpc.start-timeout-ms <eth-rpc.start-timeout-ms>
          The amount of time to wait upon startup before considering that the node timed out

          [default: 5000]

Genesis Configuration:
      --genesis.path <genesis.path>
          Specifies the path of the genesis file to use for the nodes that are started.

          This is expected to be the path of a JSON geth genesis file.

Wallet Configuration:
      --wallet.default-private-key <DEFAULT_KEY>
          The private key of the default signer

          [default: 0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d]

      --wallet.additional-keys <ADDITIONAL_KEYS>
          This argument controls which private keys the nodes should have access to and be added to its wallet signers. With a value of N, private keys (0, N] will be added to the signer set of the node

          [default: 100000]

Concurrency Configuration:
      --concurrency.number-of-nodes <NUMBER_OF_NODES>
          Determines the amount of nodes that will be spawned for each chain

          [default: 5]

      --concurrency.number-of-threads <NUMBER_OF_THREADS>
          Determines the amount of tokio worker threads that will will be used

          [default: 16]

      --concurrency.number-of-concurrent-tasks <NUMBER_CONCURRENT_TASKS>
          Determines the amount of concurrent tasks that will be spawned to run tests.

          Defaults to 10 x the number of nodes.

      --concurrency.ignore-concurrency-limit
          Determines if the concurrency limit should be ignored or not

Compilation Configuration:
      --compilation.invalidate-cache
          Controls if the compilation cache should be invalidated or not

Report Configuration:
      --report.include-compiler-input
          Controls if the compiler input is included in the final report

      --report.include-compiler-output
          Controls if the compiler output is included in the final report
```

To run tests with this tool you need a corpus JSON file that defines the tests included in the corpus. The simplest corpus file looks like the following:

```json
{
  "name": "MatterLabs Solidity Simple, Complex, and Semantic Tests",
  "path": "resolc-compiler-tests/fixtures/solidity"
}
```

> [!NOTE]  
> Note that the tests can be found in the [`resolc-compiler-tests`](https://github.com/paritytech/resolc-compiler-tests) repository.

The above corpus file instructs the tool to look for all of the test cases contained within all of the metadata files of the specified directory.

The simplest command to run this tool is the following:

```bash
RUST_LOG="info" cargo run --release -- execute-tests \
    --platform geth-evm-solc \
    --corpus corp.json \
    --working-directory workdir \
    --concurrency.number-of-nodes 5 \
    --concurrency.ignore-concurrency-limit \
    > logs.log \
    2> output.log
```

The above command will run the tool executing every one of the tests discovered in the path specified in the corpus file. All of the logs from the execution will be persisted in the `logs.log` file and all of the output of the tool will be persisted to the `output.log` file. If all that you're looking for is to run the tool and check which tests succeeded and failed, then the `output.log` file is what you need to be looking at. However, if you're contributing the to the tool then the `logs.log` file will be very valuable.

If you only want to run a subset of tests, then you can specify that in your corpus file. The following is an example:

```json
{
  "name": "MatterLabs Solidity Simple, Complex, and Semantic Tests",
  "paths": [
    "path/to/a/single/metadata/file/I/want/to/run.json",
    "path/to/a/directory/to/find/all/metadata/files/within"
  ]
}
```

<details>
<summary>User Managed Nodes</summary>

> [!CAUTION]
> This is an advanced feature of the tool and could lead test successes or failures to not be reproducible. Please use this feature with caution and only if you understand the implications of running your own node instead of having the framework manage your nodes.

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
  --corpus ./corpus.json \
  --working-directory ./workdir \
  --concurrency.number-of-nodes 1 \
  --concurrency.number-of-concurrent-tasks 5 \
  --revive-dev-node.existing-rpc-url "http://localhost:8545" \
  > logs.log
```

</details>
