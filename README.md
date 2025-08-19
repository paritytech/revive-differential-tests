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

All of the above need to be installed and available in the path in order for the tool to work.

## Running The Tool

This tool is being updated quite frequently. Therefore, it's recommended that you don't install the tool and then run it, but rather that you run it from the root of the directory using `cargo run --release`. The help command of the tool gives you all of the information you need to know about each of the options and flags that the tool offers.

```bash
$ cargo run --release -- --help
Usage: retester [OPTIONS]

Options:
  -s, --solc <SOLC>
          The `solc` version to use if the test didn't specify it explicitly

          [default: 0.8.29]

      --wasm
          Use the Wasm compiler versions

  -r, --resolc <RESOLC>
          The path to the `resolc` executable to be tested.

          By default it uses the `resolc` binary found in `$PATH`.

          If `--wasm` is set, this should point to the resolc Wasm ile.

          [default: resolc]

  -c, --corpus <CORPUS>
          A list of test corpus JSON files to be tested

  -w, --workdir <WORKING_DIRECTORY>
          A place to store temporary artifacts during test execution.

          Creates a temporary dir if not specified.

  -g, --geth <GETH>
          The path to the `geth` executable.

          By default it uses `geth` binary found in `$PATH`.

          [default: geth]

      --geth-start-timeout <GETH_START_TIMEOUT>
          The maximum time in milliseconds to wait for geth to start

          [default: 5000]

      --genesis <GENESIS_FILE>
          Configure nodes according to this genesis.json file

          [default: genesis.json]

  -a, --account <ACCOUNT>
          The signing account private key

          [default: 0x4f3edf983ac636a65a842ce7c78d9aa706d3b113bce9c46f30d7d21715b23b1d]

      --private-keys-count <PRIVATE_KEYS_TO_ADD>
          This argument controls which private keys the nodes should have access to and be added to its wallet signers. With a value of N, private keys (0, N] will be added to the signer set of the node

          [default: 100000]

  -l, --leader <LEADER>
          The differential testing leader node implementation

          [default: geth]

          Possible values:
          - geth:        The go-ethereum reference full node EVM implementation
          - kitchensink: The kitchensink runtime provides the PolkaVM (PVM) based node implentation

  -f, --follower <FOLLOWER>
          The differential testing follower node implementation

          [default: kitchensink]

          Possible values:
          - geth:        The go-ethereum reference full node EVM implementation
          - kitchensink: The kitchensink runtime provides the PolkaVM (PVM) based node implentation

      --compile-only <COMPILE_ONLY>
          Only compile against this testing platform (doesn't execute the tests)

          Possible values:
          - geth:        The go-ethereum reference full node EVM implementation
          - kitchensink: The kitchensink runtime provides the PolkaVM (PVM) based node implentation

      --number-of-nodes <NUMBER_OF_NODES>
          Determines the amount of nodes that will be spawned for each chain

          [default: 1]

      --number-of-threads <NUMBER_OF_THREADS>
          Determines the amount of tokio worker threads that will will be used

          [default: 16]

      --number-concurrent-tasks <NUMBER_CONCURRENT_TASKS>
          Determines the amount of concurrent tasks that will be spawned to run tests. Defaults to 10 x the number of nodes

  -e, --extract-problems
          Extract problems back to the test corpus

  -k, --kitchensink <KITCHENSINK>
          The path to the `kitchensink` executable.

          By default it uses `substrate-node` binary found in `$PATH`.

          [default: substrate-node]

  -p, --eth_proxy <ETH_PROXY>
          The path to the `eth_proxy` executable.

          By default it uses `eth-rpc` binary found in `$PATH`.

          [default: eth-rpc]

  -i, --invalidate-compilation-cache
          Controls if the compilation cache should be invalidated or not

  -h, --help
          Print help (see a summary with '-h')
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
RUST_LOG="info" cargo run --release -- \
    --corpus path_to_your_corpus_file.json \
    --workdir path_to_a_temporary_directory_to_cache_things_in \
    --number-of-nodes 5 \
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
