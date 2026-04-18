use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::Address;
use revive_dt_common::types::VersionOrRequirement;
use revive_dt_compiler::{
    Compiler, ModeOptimizerLevel, ModeOptimizerSetting,
    revive_resolc::{Resolc, ResolcKind},
    solc::{Solc, SolcKind},
};
use revive_dt_config::{
    Compile, HasResolcConfiguration, HasSolcConfiguration, HasWorkingDirectoryConfiguration, Test,
};
use semver::Version;

/// The environment variable to set to run the resolc Wasm tests.
/// The set value should be the absolute path to the resolc
/// JavaScript file co-located with the Wasm module.
const RESOLC_JS_PATH_ENV_NAME: &str = "RETESTER_RESOLC_JS_PATH";

/// Gets the resolc JavaScript file path from an environment variable.
/// Panics if the environment variable is not set.
fn get_resolc_js_path() -> PathBuf {
    let path = std::env::var(RESOLC_JS_PATH_ENV_NAME).unwrap_or_else(|_| {
        panic!(
            "To run resolc Wasm tests, the environment variable `{RESOLC_JS_PATH_ENV_NAME}` \
            must be set to the absolute path of the resolc JavaScript file (with the resolc \
            Wasm module co-located). Download the artifacts from https://github.com/paritytech/revive/releases. \
            To skip the test locally, run the tests with `-- --skip <name of test>`."
        )
    });

    PathBuf::from(path)
}

#[tokio::test]
async fn contracts_can_be_compiled_with_solc() {
    // Arrange
    let args = Test::default();
    let solc = Solc::new_native(
        // Clone args because this function takes ownership and drops it, which
        // would delete the temporary directory before the compiled binary is used.
        args.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

    assert_eq!(solc.kind(), SolcKind::Native);

    // Act
    let output = Compiler::new()
        .with_source("./tests/assets/array_one_element/callable.sol")
        .unwrap()
        .with_source("./tests/assets/array_one_element/main.sol")
        .unwrap()
        .try_build(&solc)
        .await;

    // Assert
    let output = output.expect("Failed to compile");
    assert_eq!(output.contracts.len(), 2);

    let main_file_contracts = output
        .contracts
        .get(
            &PathBuf::from("./tests/assets/array_one_element/main.sol")
                .canonicalize()
                .unwrap(),
        )
        .unwrap();
    let callable_file_contracts = output
        .contracts
        .get(
            &PathBuf::from("./tests/assets/array_one_element/callable.sol")
                .canonicalize()
                .unwrap(),
        )
        .unwrap();
    assert!(main_file_contracts.contains_key("Main"));
    assert!(callable_file_contracts.contains_key("Callable"));
}

async fn assert_contracts_can_be_compiled_with_resolc(
    context: impl HasSolcConfiguration
    + HasResolcConfiguration
    + HasWorkingDirectoryConfiguration
    + Clone
    + Send
    + 'static,
    expected_kind: ResolcKind,
) {
    let resolc = Resolc::new(
        // Clone context because this function takes ownership and drops it, which
        // would delete the temporary directory before the compiled binary is used.
        context.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

    assert_eq!(resolc.kind(), expected_kind);

    // Act
    let output = Compiler::new()
        .with_source("./tests/assets/array_one_element/callable.sol")
        .unwrap()
        .with_source("./tests/assets/array_one_element/main.sol")
        .unwrap()
        .try_build(&resolc)
        .await;

    // Assert
    let output = output.expect("Failed to compile");
    assert_eq!(output.contracts.len(), 2);

    let main_file_contracts = output
        .contracts
        .get(
            &PathBuf::from("./tests/assets/array_one_element/main.sol")
                .canonicalize()
                .unwrap(),
        )
        .unwrap();
    let callable_file_contracts = output
        .contracts
        .get(
            &PathBuf::from("./tests/assets/array_one_element/callable.sol")
                .canonicalize()
                .unwrap(),
        )
        .unwrap();
    assert!(main_file_contracts.contains_key("Main"));
    assert!(callable_file_contracts.contains_key("Callable"));
}

#[tokio::test]
async fn contracts_can_be_compiled_with_resolc_native() {
    assert_contracts_can_be_compiled_with_resolc(Test::default(), ResolcKind::Native).await;
}

#[tokio::test]
async fn contracts_can_be_compiled_with_resolc_wasm() {
    let mut context = Compile::default();
    context.resolc.path = get_resolc_js_path();
    assert_contracts_can_be_compiled_with_resolc(context, ResolcKind::Wasm).await;
}

/// Asserts that bytecode differs across optimization modes in order to verify
/// that the optimization settings are honored when instantiating the compiler.
#[tokio::test]
async fn bytecode_differs_across_optimization_modes() {
    let context = Test::default();
    let resolc = Resolc::new(
        context.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

    let mut bytecodes = HashMap::new();
    for level in [
        ModeOptimizerLevel::M0,
        ModeOptimizerLevel::M3,
        ModeOptimizerLevel::Mz,
    ] {
        let output = Compiler::new()
            .with_optimization(ModeOptimizerSetting {
                solc_optimizer_enabled: true,
                level,
            })
            .with_source("./tests/assets/loop_with_array/main.sol")
            .unwrap()
            .try_build(&resolc)
            .await
            .expect("Failed to compile");

        let contracts = output
            .contracts
            .get(
                &PathBuf::from("./tests/assets/loop_with_array/main.sol")
                    .canonicalize()
                    .unwrap(),
            )
            .unwrap();

        let (bytecode, _abi) = contracts.get("Test").unwrap();
        if let Some(previous_level) = bytecodes.get(bytecode) {
            panic!("{previous_level:?} and {level:?} produced identical bytecode: {bytecode}");
        }
        bytecodes.insert(bytecode.clone(), level);
    }
    assert_eq!(bytecodes.len(), 3);
}

/// Asserts that compiling a contract that calls an external library from two
/// different absolute paths produces identical bytecode.
#[tokio::test]
async fn bytecode_is_source_path_independent_when_calling_external_library() {
    let context = Test::default();
    let resolc = Resolc::new(
        context.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

    let fixture_directory = PathBuf::from("./tests/assets/library_call");

    // Two temporary directories simulate different platform working directories.
    let directory_a = tempfile::tempdir().unwrap();
    let directory_b = tempfile::tempdir().unwrap();

    // Copy the fixtures into each directory.
    for directory in [directory_a.path(), directory_b.path()] {
        for entry in std::fs::read_dir(&fixture_directory).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), directory.join(entry.file_name())).unwrap();
        }
    }

    assert_ne!(
        directory_a.path(),
        directory_b.path(),
        "Temporary directories must differ"
    );

    let dummy_library_address = Address::with_last_byte(1);
    let mut bytecodes = Vec::new();

    for directory in [directory_a.path(), directory_b.path()] {
        let contract_path = directory.join("main.sol");
        let library_path = directory.join("lib.sol");
        let output = Compiler::new()
            .with_base_path(directory.to_path_buf())
            .with_allow_path(directory)
            .with_source(&contract_path)
            .unwrap()
            .with_library(&library_path, "MathLib", dummy_library_address)
            .try_build(&resolc)
            .await
            .expect("Failed to compile");

        let contracts = output
            .contracts
            .get(&contract_path.canonicalize().unwrap())
            .unwrap();
        let (bytecode, _abi) = contracts.get("Main").unwrap();
        bytecodes.push(bytecode.clone());
    }

    assert_eq!(bytecodes.len(), 2);
    assert_eq!(
        bytecodes[0], bytecodes[1],
        "The bytecode should be identical regardless of path"
    );
}
