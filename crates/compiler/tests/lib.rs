use std::collections::HashMap;
use std::path::PathBuf;

use revive_dt_common::types::VersionOrRequirement;
use revive_dt_compiler::{
    Compiler, ModeOptimizerLevel, ModeOptimizerSetting, revive_resolc::Resolc, solc::Solc,
};
use revive_dt_config::Test;
use semver::Version;

#[tokio::test]
async fn contracts_can_be_compiled_with_solc() {
    // Arrange
    let args = Test::default();
    let solc = Solc::new(
        // Clone args because this function takes ownership and drops it, which
        // would delete the temporary directory before the compiled binary is used.
        args.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

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

#[tokio::test]
async fn contracts_can_be_compiled_with_resolc() {
    // Arrange
    let args = Test::default();
    let resolc = Resolc::new(
        // Clone args because this function takes ownership and drops it, which
        // would delete the temporary directory before the compiled binary is used.
        args.clone(),
        VersionOrRequirement::Version(Version::new(0, 8, 30)),
    )
    .await
    .unwrap();

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

/// Asserts that bytecode differs across optimization modes in order to verify
/// that the optimization settings are honored when instantiating the compiler.
#[tokio::test]
async fn bytecode_differs_across_optimization_modes() {
    let args = Test::default();
    let resolc = Resolc::new(
        args.clone(),
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
