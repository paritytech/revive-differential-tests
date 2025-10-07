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

## Logging

The ml-test-runner uses the `tracing` crate for logging. Set the `RUST_LOG` environment variable to control log output:

- `RUST_LOG=info` - Shows high-level progress (file discovery, node startup, test execution)
- `RUST_LOG=debug` - Shows detailed execution flow (compilation, driver creation, step execution)
- `RUST_LOG=trace` - Shows very detailed tracing (mostly from dependencies)

Logs are written to stderr, while test results are written to stdout for easy filtering.

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

## Implementation Status

### ✅ Completed
- CLI argument parsing with full configuration options
- File discovery (single file, corpus, recursive directory walk)
- Cached-passed tracking system
- Cargo-test-style output formatting
- Contract compilation with caching
- Platform node management and spawning
- Library deployment and linking
- Full case execution using Driver pattern
- Test file discovery and metadata loading
- Pass/fail tracking and caching
- Output formatting and summary generation
- Error handling and bail behavior
- Optional node startup with `--start-platform` flag
- Compile-only mode (default) for fast validation
- Full execution mode (with `--start-platform`) for actual testing
- Tracing/logging support via `RUST_LOG`

### 🚧 TODO
- Additional optimizations and performance tuning
- Support for custom working directories

## Implementation Details

The ml-test-runner is a **simplified, single-platform test runner** that shares core components with the main differential testing tool:

- **Compilation**: Uses the shared `CachedCompiler` from `revive-dt-core` that stores compilation artifacts to avoid recompiling
- **Library Deployment**: Automatically deploys library contracts when needed and links them
- **Test Execution**: Uses the shared `Driver` from `revive-dt-core::differential_tests` to execute test cases on the configured platform
- **Node Management**: Optionally spawns and manages blockchain nodes (when `--start-platform` is used)
- **Single Platform**: Unlike the main tool which does differential testing (comparing multiple platforms), `ml-test-runner` executes against a single platform
- **Two Modes**:
  - **Compile-only mode** (default): Fast validation that contracts compile correctly, no node required
  - **Full execution mode** (`--start-platform`): Spawns a node and executes all test steps with assertions
- **Tracing**: Full logging support via `tracing` and `tracing-subscriber` crates

The implementation is clean, focused code that reuses battle-tested components from `revive-dt-core`. This ensures consistency while maintaining a lean codebase optimized for ML pipeline integration.

## Building

```bash
cargo build --release -p ml-test-runner
```

The binary will be available at `target/release/ml-test-runner`.
