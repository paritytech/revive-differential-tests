# ML Test Runner

A test runner for executing Revive differential tests file-by-file with cargo-test-style output.

This is similar to the `retester` binary but designed for ML-based test execution with a focus on:
- Running tests file-by-file (rather than in bulk)
- Caching passed tests to skip them in future runs
- Providing cargo-test-style output for easy integration with ML pipelines
- Single platform testing (rather than differential testing)

## Features

- **File-by-file execution**: Run tests on individual `.sol` files, corpus files (`.json`), or recursively walk directories
- **Cached results**: Skip tests that have already passed using `--cached-passed`
- **Fail fast**: Stop on first failure with `--bail`
- **Cargo-like output**: Familiar test output format with colored pass/fail indicators
- **Platform support**: Test against `geth` or `kitchensink` platforms

## Usage

```bash
# Run a single .sol file (compile-only mode, default)
./ml-test-runner path/to/test.sol --platform geth

# Run all tests in a corpus file
./ml-test-runner path/to/corpus.json --platform kitchensink

# Walk a directory recursively for .sol files
./ml-test-runner path/to/tests/ --platform geth

# Use cached results and bail on first failure
./ml-test-runner path/to/tests/ --cached-passed ./cache.txt --bail

# Start the platform and execute tests (full mode)
./ml-test-runner path/to/tests/ --platform geth --start-platform

# Enable verbose logging (info, debug, or trace level)
RUST_LOG=info ./ml-test-runner path/to/tests/
RUST_LOG=debug ./ml-test-runner path/to/tests/ --start-platform
RUST_LOG=trace ./ml-test-runner path/to/tests/ --start-platform
```

## Arguments

- `<PATH>` - Path to test file (`.sol`), corpus file (`.json`), or folder of `.sol` files
- `--cached-passed <FILE>` - File to track tests that have already passed
- `--bail` - Stop after the first file failure
- `--platform <PLATFORM>` - Platform to test against (`geth`, `kitchensink`, or `zombienet`, default: `geth`)
- `--start-platform` - Start the platform and execute tests (default: `false`, compile-only mode)

## Output Format

The runner produces cargo-test-style output:

```
test path/to/test1.sol ... ok
test path/to/test2.sol ... FAILED
test path/to/test3.sol ... cached

failures:

---- path/to/test2.sol ----
Error: ...

test result: FAILED. 1 passed; 1 failed; 1 cached; finished in 2.34s
```

## Building

```bash
cargo build --release -p ml-test-runner
```

The binary will be available at `target/release/ml-test-runner`.
