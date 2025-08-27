//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{collections::HashMap, path::PathBuf, process::Stdio};

use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::Arguments;

use super::constants::SOLC_VERSION_SUPPORTING_VIA_YUL_IR;
use super::utils;
use crate::{
    CompilerInput, CompilerOutput, ModeOptimizerSetting, ModePipeline, SolcCompilerInformation,
    SolidityCompiler,
};

use anyhow::Context;
use foundry_compilers_artifacts::{
    output_selection::{
        BytecodeOutputSelection, ContractOutputSelection, EvmOutputSelection, OutputSelection,
    },
    solc::CompilerOutput as SolcOutput,
    solc::*,
};
use semver::Version;
use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand};

#[derive(Debug)]
pub struct Solc {
    // Where to cache artifacts.
    cache_directory: PathBuf,
    // We'll use this version when no explicit version requirement
    // is given in the test mode.
    solc_version: Version,
}

impl SolidityCompiler for Solc {
    type Options = ();

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
            revert_string_handling,
        }: CompilerInput,
        _: Self::Options,
    ) -> anyhow::Result<CompilerOutput> {
        let solc_version_req = solc_version
            .unwrap_or_else(|| VersionOrRequirement::version_to_requirement(&self.solc_version));
        let solc_path =
            revive_dt_solc_binaries::download_solc(&self.cache_directory, solc_version_req, false)
                .await?;
        let solc_version = utils::solc_version(&solc_path).await?;

        let compiler_supports_via_ir =
            utils::solc_version(&solc_path).await? >= SOLC_VERSION_SUPPORTING_VIA_YUL_IR;

        // Be careful to entirely omit the viaIR field if the compiler does not support it,
        // as it will error if you provide fields it does not know about. Because
        // `supports_mode` is called prior to instantiating a compiler, we should never
        // ask for something which is invalid.
        let via_ir = match (pipeline, compiler_supports_via_ir) {
            (pipeline, true) => pipeline.map(|p| p.via_yul_ir()),
            (_pipeline, false) => None,
        };

        let input = SolcInput {
            language: SolcLanguage::Solidity,
            sources: Sources(
                sources
                    .into_iter()
                    .map(|(source_path, source_code)| (source_path, Source::new(source_code)))
                    .collect(),
            ),
            settings: Settings {
                optimizer: Optimizer {
                    enabled: optimization.map(|o| o.optimizations_enabled()),
                    details: Some(Default::default()),
                    ..Default::default()
                },
                output_selection: OutputSelection::common_output_selection(
                    [
                        ContractOutputSelection::Abi,
                        ContractOutputSelection::Evm(EvmOutputSelection::ByteCode(
                            BytecodeOutputSelection::Object,
                        )),
                    ]
                    .into_iter()
                    .map(|item| item.to_string()),
                ),
                evm_version: evm_version.map(|version| version.to_string().parse().unwrap()),
                via_ir,
                libraries: Libraries {
                    libs: libraries
                        .into_iter()
                        .map(|(file_path, libraries)| {
                            (
                                file_path,
                                libraries
                                    .into_iter()
                                    .map(|(library_name, library_address)| {
                                        (library_name, library_address.to_string())
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                },
                debug: revert_string_handling.map(|revert_string_handling| DebuggingSettings {
                    revert_strings: match revert_string_handling {
                        crate::RevertString::Default => Some(RevertStrings::Default),
                        crate::RevertString::Debug => Some(RevertStrings::Debug),
                        crate::RevertString::Strip => Some(RevertStrings::Strip),
                        crate::RevertString::VerboseDebug => Some(RevertStrings::VerboseDebug),
                    },
                    debug_info: Default::default(),
                }),
                ..Default::default()
            },
        };

        let mut command = AsyncCommand::new(&solc_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to spawn solc at {}", solc_path.display()))?;

        let stdin = child.stdin.as_mut().expect("should be piped");
        let serialized_input = serde_json::to_vec(&input)
            .context("Failed to serialize Standard JSON input for solc")?;
        stdin
            .write_all(&serialized_input)
            .await
            .context("Failed to write Standard JSON to solc stdin")?;
        let output = child
            .wait_with_output()
            .await
            .context("Failed while waiting for solc process to finish")?;

        if !output.status.success() {
            let json_in = serde_json::to_string_pretty(&input)
                .context("Failed to pretty-print Standard JSON input for logging")?;
            let message = String::from_utf8_lossy(&output.stderr);
            tracing::error!(
                status = %output.status,
                message = %message,
                json_input = json_in,
                "Compilation using solc failed"
            );
            anyhow::bail!("Compilation failed with an error: {message}");
        }

        let parsed = serde_json::from_slice::<SolcOutput>(&output.stdout)
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse resolc JSON output: {e}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            })
            .context("Failed to parse solc standard JSON output")?;

        // Detecting if the compiler output contained errors and reporting them through logs and
        // errors instead of returning the compiler output that might contain errors.
        for error in parsed.errors.iter() {
            if error.severity == Severity::Error {
                tracing::error!(?error, ?input, "Encountered an error in the compilation");
                anyhow::bail!("Encountered an error in the compilation: {error}")
            }
        }

        tracing::debug!(
            output = %String::from_utf8_lossy(&output.stdout).to_string(),
            "Compiled successfully"
        );

        let mut compiled_contracts = HashMap::<PathBuf, HashMap<_, _>>::new();
        for (contract_path, contracts) in parsed.contracts {
            let map = compiled_contracts
                .entry(contract_path.canonicalize().with_context(|| {
                    format!(
                        "Failed to canonicalize contract path {}",
                        contract_path.display()
                    )
                })?)
                .or_default();
            for (contract_name, contract_info) in contracts.into_iter() {
                let source_code = contract_info
                    .evm
                    .and_then(|evm| evm.bytecode)
                    .map(|bytecode| match bytecode.object {
                        BytecodeObject::Bytecode(bytecode) => bytecode.to_string(),
                        BytecodeObject::Unlinked(unlinked) => unlinked,
                    })
                    .context("Unexpected - contract compiled with solc has no source code")?;
                let abi = contract_info
                    .abi
                    .context("Unexpected - contract compiled with solc as no ABI")?;
                map.insert(contract_name, (source_code, abi));
            }
        }

        Ok(CompilerOutput {
            solc: SolcCompilerInformation {
                version: solc_version,
                path: solc_path,
            },
            contracts: compiled_contracts,
        })
    }

    fn new(config: &Arguments) -> Self {
        Self {
            cache_directory: config.directory().to_path_buf(),
            solc_version: config.solc.clone(),
        }
    }

    fn supports_mode(_optimize_setting: ModeOptimizerSetting, pipeline: ModePipeline) -> bool {
        // solc 0.8.13 and above supports --via-ir, and less than that does not. Thus, we support mode E
        // (ie no Yul IR) in either case, but only support Y (via Yul IR) if the compiler is new enough.
        pipeline == ModePipeline::ViaEVMAssembly || pipeline == ModePipeline::ViaYulIR
    }
}
