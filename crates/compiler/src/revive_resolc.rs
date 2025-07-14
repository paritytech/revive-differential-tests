//! Implements the [SolidityCompiler] trait with `resolc` for
//! compiling contracts to PolkaVM (PVM) bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_solc_json_interface::SolcStandardJsonOutput;

/// A wrapper around the `resolc` binary, emitting PVM-compatible bytecode.
#[derive(Debug)]
pub struct Resolc {
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
}

impl SolidityCompiler for Resolc {
    type Options = Vec<String>;

    #[tracing::instrument(level = "debug", ret)]
    fn build(
        &self,
        input: CompilerInput<Self::Options>,
    ) -> anyhow::Result<CompilerOutput<Self::Options>> {
        let mut child = Command::new(&self.resolc_path)
            .arg("--standard-json")
            .args(&input.extra_options)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin_pipe = child.stdin.as_mut().expect("stdin must be piped");
        serde_json::to_writer(stdin_pipe, &input.input)?;

        let json_in = serde_json::to_string_pretty(&input.input)?;

        let output = child.wait_with_output()?;
        let stdout = output.stdout;
        let stderr = output.stderr;

        if !output.status.success() {
            let message = String::from_utf8_lossy(&stderr);
            tracing::error!(
                "resolc failed exit={} stderr={} JSON-in={} ",
                output.status,
                &message,
                json_in,
            );
            return Ok(CompilerOutput {
                input,
                output: Default::default(),
                error: Some(message.into()),
            });
        }

        let mut parsed =
            serde_json::from_slice::<SolcStandardJsonOutput>(&stdout).map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse resolc JSON output: {e}\nstderr: {}",
                    String::from_utf8_lossy(&stderr)
                )
            })?;

        // We need to do some post processing on the output to make it in the same format that solc
        // outputs. More specifically, for each contract, the `.metadata` field should be replaced
        // with the `.metadata.solc_metadata` field which contains the ABI and other information
        // about the compiled contracts. We do this because we do not want any downstream logic to
        // need to differentiate between which compiler is being used when extracting the ABI of the
        // contracts.
        if let Some(ref mut contracts) = parsed.contracts {
            for (contract_path, contracts_map) in contracts.iter_mut() {
                for (contract_name, contract_info) in contracts_map.iter_mut() {
                    let Some(metadata) = contract_info.metadata.take() else {
                        continue;
                    };

                    // Get the `solc_metadata` in the metadata of the contract.
                    let Some(solc_metadata) = metadata
                        .get("solc_metadata")
                        .and_then(|metadata| metadata.as_str())
                    else {
                        tracing::error!(
                            contract_path,
                            contract_name,
                            metadata = serde_json::to_string(&metadata).unwrap(),
                            "Encountered a contract compiled with resolc that has no solc_metadata"
                        );
                        anyhow::bail!(
                            "Contract {} compiled with resolc that has no solc_metadata",
                            contract_name
                        );
                    };

                    // Replace the original metadata with the new solc_metadata.
                    contract_info.metadata =
                        Some(serde_json::Value::String(solc_metadata.to_string()));
                }
            }
        }

        tracing::debug!(
            output = %serde_json::to_string(&parsed).unwrap(),
            "Compiled successfully"
        );

        Ok(CompilerOutput {
            input,
            output: parsed,
            error: None,
        })
    }

    fn new(resolc_path: PathBuf) -> Self {
        Resolc { resolc_path }
    }

    fn get_compiler_executable(
        config: &Arguments,
        _version: semver::Version,
    ) -> anyhow::Result<PathBuf> {
        if !config.resolc.as_os_str().is_empty() {
            return Ok(config.resolc.clone());
        }

        Ok(PathBuf::from("resolc"))
    }
}
