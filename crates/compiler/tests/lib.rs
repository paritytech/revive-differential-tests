use std::path::PathBuf;

use revive_dt_compiler::{Compiler, SolidityCompiler, revive_resolc::Resolc, solc::Solc};
use revive_dt_config::Arguments;
use semver::Version;

#[tokio::test]
async fn contracts_can_be_compiled_with_solc() {
    // Arrange
    let args = Arguments::default();
    let compiler_path = Solc::get_compiler_executable(&args, Version::new(0, 8, 30)).unwrap();

    // Act
    let output = Compiler::<Solc>::new()
        .with_source("./tests/assets/array_one_element/callable.sol")
        .unwrap()
        .with_source("./tests/assets/array_one_element/main.sol")
        .unwrap()
        .try_build(compiler_path)
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
    let args = Arguments::default();
    let compiler_path = Resolc::get_compiler_executable(&args, Version::new(0, 8, 30)).unwrap();

    // Act
    let output = Compiler::<Resolc>::new()
        .with_source("./tests/assets/array_one_element/callable.sol")
        .unwrap()
        .with_source("./tests/assets/array_one_element/main.sol")
        .unwrap()
        .try_build(compiler_path)
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
