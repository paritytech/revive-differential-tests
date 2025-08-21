//! Implements the [SolidityCompiler] trait with `resolc` for
//! compiling contracts to PolkaVM (PVM) bytecode.

use std::{path::PathBuf, process::Stdio};

use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::Arguments;
use revive_solc_json_interface::{
    SolcStandardJsonInput, SolcStandardJsonInputLanguage, SolcStandardJsonInputSettings,
    SolcStandardJsonInputSettingsOptimizer, SolcStandardJsonInputSettingsSelection,
    SolcStandardJsonOutput,
};

use super::constants::SOLC_VERSION_SUPPORTING_VIA_YUL_IR;
use super::utils;
use crate::{CompilerInput, CompilerOutput, ModeOptimizerSetting, ModePipeline, SolidityCompiler};

use alloy::json_abi::JsonAbi;
use anyhow::Context;
use semver::Version;
use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand};

/// A wrapper around the `resolc` binary, emitting PVM-compatible bytecode.
#[derive(Debug)]
pub struct Resolc {
    // Enable wasm compilation.
    wasm: bool,
    // Where to cache artifacts.
    cache_directory: PathBuf,
    // We'll use this version when no explicit version
    // requirement is given in the test mode.
    solc_version: Version,
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
}

impl SolidityCompiler for Resolc {
    type Options = Vec<String>;

    #[tracing::instrument(level = "debug", ret)]
    async fn build(
        &self,
        CompilerInput {
            pipeline,
            optimization,
            solc_version,
            evm_version,
            allow_paths,
            base_path,
            sources,
            libraries,
            // TODO: this is currently not being handled since there is no way to pass it into
            // resolc. So, we need to go back to this later once it's supported.
            revert_string_handling: _,
        }: CompilerInput,
        additional_options: Self::Options,
    ) -> anyhow::Result<CompilerOutput> {
        if !matches!(pipeline, None | Some(ModePipeline::ViaYulIR)) {
            anyhow::bail!(
                "Resolc only supports the Y (via Yul IR) pipeline, but the provided pipeline is {pipeline:?}"
            );
        }

        let solc_version_req = solc_version
            .unwrap_or_else(|| VersionOrRequirement::version_to_requirement(&self.solc_version));
        let solc_path = revive_dt_solc_binaries::download_solc(
            &self.cache_directory,
            solc_version_req,
            self.wasm,
        )
        .await?;
        let solc_version = utils::solc_version(&solc_path).await?;

        if solc_version < SOLC_VERSION_SUPPORTING_VIA_YUL_IR {
            anyhow::bail!(
                "We are trying to run the test with solc version {solc_version}, but require {SOLC_VERSION_SUPPORTING_VIA_YUL_IR} or greater"
            );
        }

        let input = SolcStandardJsonInput {
            language: SolcStandardJsonInputLanguage::Solidity,
            sources: sources
                .into_iter()
                .map(|(path, source)| (path.display().to_string(), source.into()))
                .collect(),
            settings: SolcStandardJsonInputSettings {
                evm_version,
                libraries: Some(
                    libraries
                        .into_iter()
                        .map(|(source_code, libraries_map)| {
                            (
                                source_code.display().to_string(),
                                libraries_map
                                    .into_iter()
                                    .map(|(library_ident, library_address)| {
                                        (library_ident, library_address.to_string())
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                ),
                remappings: None,
                output_selection: Some(SolcStandardJsonInputSettingsSelection::new_required()),
                via_ir: Some(true),
                optimizer: SolcStandardJsonInputSettingsOptimizer::new(
                    optimization
                        .unwrap_or(ModeOptimizerSetting::M0)
                        .optimizations_enabled(),
                    None,
                    &Version::new(0, 0, 0),
                    false,
                ),
                metadata: None,
                polkavm: None,
            },
        };

        let mut command = AsyncCommand::new(&self.resolc_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--solc")
            .arg(&solc_path)
            .arg("--standard-json");

        if let Some(ref base_path) = base_path {
            command.arg("--base-path").arg(base_path);
        }
        if !allow_paths.is_empty() {
            command.arg("--allow-paths").arg(
                allow_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        let mut child = command.spawn()?;

        let stdin_pipe = child.stdin.as_mut().expect("stdin must be piped");
        let serialized_input = serde_json::to_vec(&input)?;
        stdin_pipe.write_all(&serialized_input).await?;

        let output = child.wait_with_output().await?;
        let stdout = output.stdout;
        let stderr = output.stderr;

        if !output.status.success() {
            let json_in = serde_json::to_string_pretty(&input)?;
            let message = String::from_utf8_lossy(&stderr);
            tracing::error!(
                status = %output.status,
                message = %message,
                json_input = json_in,
                "Compilation using resolc failed"
            );
            anyhow::bail!("Compilation failed with an error: {message}");
        }

        let parsed = serde_json::from_slice::<SolcStandardJsonOutput>(&stdout).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse resolc JSON output: {e}\nstderr: {}",
                String::from_utf8_lossy(&stderr)
            )
        })?;

        tracing::debug!(
            output = %serde_json::to_string(&parsed).unwrap(),
            "Compiled successfully"
        );

        // Detecting if the compiler output contained errors and reporting them through logs and
        // errors instead of returning the compiler output that might contain errors.
        for error in parsed.errors.iter().flatten() {
            if error.severity == "error" {
                tracing::error!(
                    ?error,
                    ?input,
                    output = %serde_json::to_string(&parsed).unwrap(),
                    "Encountered an error in the compilation"
                );
                anyhow::bail!("Encountered an error in the compilation: {error}")
            }
        }

        let Some(contracts) = parsed.contracts else {
            anyhow::bail!("Unexpected error - resolc output doesn't have a contracts section");
        };

        let mut compiler_output = CompilerOutput::default();
        for (source_path, contracts) in contracts.into_iter() {
            let source_path = PathBuf::from(source_path).canonicalize()?;

            let map = compiler_output.contracts.entry(source_path).or_default();
            for (contract_name, contract_information) in contracts.into_iter() {
                let bytecode = contract_information
                    .evm
                    .and_then(|evm| evm.bytecode.clone())
                    .context("Unexpected - Contract compiled with resolc has no bytecode")?;
                let abi = contract_information
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.as_object())
                    .and_then(|metadata| metadata.get("solc_metadata"))
                    .and_then(|solc_metadata| solc_metadata.as_str())
                    .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                    .and_then(|metadata| {
                        metadata.get("output").and_then(|output| {
                            output
                                .get("abi")
                                .and_then(|abi| serde_json::from_value::<JsonAbi>(abi.clone()).ok())
                        })
                    })
                    .context(
                        "Unexpected - Failed to get the ABI for a contract compiled with resolc",
                    )?;
                map.insert(contract_name, (bytecode.object, abi));
            }
        }

        Ok(compiler_output)
    }

    fn new(config: &Arguments) -> Self {
        Resolc {
            wasm: config.wasm,
            cache_directory: config.directory().to_path_buf(),
            solc_version: config.solc.clone(),
            resolc_path: config.resolc.clone(),
        }
    }

    fn supports_mode(_optimize_setting: ModeOptimizerSetting, pipeline: ModePipeline) -> bool {
        // We only support the Y (IE compile via Yul IR) mode here. We must always compile
        // via Yul IR as resolc needs this to translate to LLVM IR and then RISCV.
        pipeline == ModePipeline::ViaYulIR
    }
}
