# revive-differential-tests

The revive differential testing framework allows to define smart contract tests in a declarative manner in order to compile and execute them against different Ethereum-compatible blockchain implmentations. This is useful to:
- Analyze observable differences in contract compilation and execution across different blockchain implementations, including contract storage, account balances, transaction output and emitted events on a per-transaction base.
- Collect and compare benchmark metrics such as code size, gas usage or transaction throughput per seconds (TPS) of different blockchain implementations.
- Ensure reproducible contract builds across multiple compiler implementations or multiple host platforms.
- Implement end-to-end regression tests for Ethereum-compatible smart contract stacks.

# Declarative test format

For now, the format used to write tests is the [matter-labs era compiler format](https://github.com/matter-labs/era-compiler-tests?tab=readme-ov-file#matter-labs-simplecomplex-format). This allows us to re-use many tests from their corpora.

# Dependencies

The following is needed to execute `geth` vs. substrate node differential tests:
- [`geth` node](https://github.com/ethereum/go-ethereum/)
- `kitchensink` node and revive ETH RPC binary
- [`solc` compiler](https://github.com/ethereum/solidity)
- [`resolc` compiler](https://github.com/paritytech/revive)

For the compilers and `geth` node please check the project help on how to obtain them.

To install the `kitchensink` node and the ETH RPC binary:
```
git submodule update --init --recursive
cd polkadot-sdk
cargo install --locked --force --profile=production --path substrate/bin/node/cli --bin substrate-node --features cli
cargo install --path substrate/frame/revive/rpc --bin eth-rpc
```

# The `retester` utility

The `retester` helper utilty is used to run the tests. To get an idea of what `retester` can do, please consults its command line help: 

```
cargo run -p revive-dt-core -- --help
```

For example, to run the [complex Solidity tests](https://github.com/matter-labs/era-compiler-tests/tree/main/solidity/complex), define a corpus structure as follows:

```json
{
    "name": "ML Solidity Complex",
    "path": "/path/to/era-compiler-tests/solidity/complex"
}
```

Assuming this to be saved in a `ml-solidity-complex.json` file, the following command will try to compile and execute the tests found inside the corpus:

```bash
RUST_LOG=debug cargo r --release -p revive-dt-core  -- --corpus ml-solidity-complex.json 
```
