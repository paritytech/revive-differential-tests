//! Implements the [SolidityCompiler] trait with `resolc` for
//! compiling contracts to PolkaVM (PVM) bytecode.

use std::{
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::{ResolcConfiguration, SolcConfiguration, WorkingDirectoryConfiguration};
use revive_solc_json_interface::{
    SolcStandardJsonInput, SolcStandardJsonInputLanguage, SolcStandardJsonInputSettings,
    SolcStandardJsonInputSettingsOptimizer, SolcStandardJsonInputSettingsSelection,
    SolcStandardJsonOutput,
};
use tracing::{Span, field::display};

use crate::{
    CompilerInput, CompilerOutput, ModeOptimizerSetting, ModePipeline, SolidityCompiler, solc::Solc,
};

use alloy::json_abi::JsonAbi;
use anyhow::{Context as _, Result};
use semver::Version;
use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand};

/// A wrapper around the `resolc` binary, emitting PVM-compatible bytecode.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Resolc(Arc<ResolcInner>);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ResolcInner {
    /// The internal solc compiler that the resolc compiler uses as a compiler frontend.
    solc: Solc,
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
}

impl Resolc {
    pub async fn new(
        context: impl AsRef<SolcConfiguration>
        + AsRef<ResolcConfiguration>
        + AsRef<WorkingDirectoryConfiguration>,
        version: impl Into<Option<VersionOrRequirement>>,
    ) -> Result<Self> {
        /// This is a cache of all of the resolc compiler objects. Since we do not currently support
        /// multiple resolc compiler versions, so our cache is just keyed by the solc compiler and
        /// its version to the resolc compiler.
        static COMPILERS_CACHE: LazyLock<DashMap<Solc, Resolc>> = LazyLock::new(Default::default);

        let resolc_configuration = AsRef::<ResolcConfiguration>::as_ref(&context);

        let solc = Solc::new(&context, version)
            .await
            .context("Failed to create the solc compiler frontend for resolc")?;

        Ok(COMPILERS_CACHE
            .entry(solc.clone())
            .or_insert_with(|| {
                Self(Arc::new(ResolcInner {
                    solc,
                    resolc_path: resolc_configuration.path.clone(),
                }))
            })
            .clone())
    }
}

impl SolidityCompiler for Resolc {
    fn version(&self) -> &Version {
        // We currently return the solc compiler version since we do not support multiple resolc
        // compiler versions.
        SolidityCompiler::version(&self.0.solc)
    }

    fn path(&self) -> &std::path::Path {
        &self.0.resolc_path
    }

    #[tracing::instrument(level = "debug", ret)]
    #[tracing::instrument(
        level = "error",
        skip_all,
        fields(
            resolc_version = %self.version(),
            solc_version = %self.0.solc.version(),
            json_in = tracing::field::Empty
        ),
        err(Debug)
    )]
    fn build(
        &self,
        CompilerInput {
            pipeline,
            optimization,
            evm_version,
            allow_paths,
            base_path,
            sources,
            libraries,
            // TODO: this is currently not being handled since there is no way to pass it into
            // resolc. So, we need to go back to this later once it's supported.
            revert_string_handling: _,
        }: CompilerInput,
    ) -> Pin<Box<dyn Future<Output = Result<CompilerOutput>> + '_>> {
        Box::pin(async move {
            if !matches!(pipeline, None | Some(ModePipeline::ViaYulIR)) {
                anyhow::bail!(
                    "Resolc only supports the Y (via Yul IR) pipeline, but the provided pipeline is {pipeline:?}"
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
            Span::current().record("json_in", display(serde_json::to_string(&input).unwrap()));

            let path = &self.0.resolc_path;
            let mut command = AsyncCommand::new(path);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .arg("--solc")
                .arg(self.0.solc.path())
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
                .with_context(|| format!("Failed to spawn resolc at {}", path.display()))?;

            let stdin_pipe = child.stdin.as_mut().expect("stdin must be piped");
            let serialized_input = serde_json::to_vec(&input)
                .context("Failed to serialize Standard JSON input for resolc")?;
            stdin_pipe
                .write_all(&serialized_input)
                .await
                .context("Failed to write Standard JSON to resolc stdin")?;

            let output = child
                .wait_with_output()
                .await
                .context("Failed while waiting for resolc process to finish")?;
            let stdout = output.stdout;
            let stderr = output.stderr;

            if !output.status.success() {
                let json_in = serde_json::to_string_pretty(&input)
                    .context("Failed to pretty-print Standard JSON input for logging")?;
                let message = String::from_utf8_lossy(&stderr);
                tracing::error!(
                    status = %output.status,
                    message = %message,
                    json_input = json_in,
                    "Compilation using resolc failed"
                );
                anyhow::bail!("Compilation failed with an error: {message}");
            }

            let parsed = serde_json::from_slice::<SolcStandardJsonOutput>(&stdout)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to parse resolc JSON output: {e}\nstderr: {}",
                        String::from_utf8_lossy(&stderr)
                    )
                })
                .context("Failed to parse resolc standard JSON output")?;

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
                let src_for_msg = source_path.clone();
                let source_path = PathBuf::from(source_path)
                    .canonicalize()
                    .with_context(|| format!("Failed to canonicalize path {src_for_msg}"))?;

                let map = compiler_output.contracts.entry(source_path).or_default();
                for (contract_name, contract_information) in contracts.into_iter() {
                    let bytecode = contract_information
                        .evm
                        .and_then(|evm| evm.bytecode.clone())
                        .context("Unexpected - Contract compiled with resolc has no bytecode")?;
                    let abi = {
                        let metadata = contract_information
                            .metadata
                            .as_ref()
                            .context("No metadata found for the contract")?;
                        let solc_metadata_str = match metadata {
                            serde_json::Value::String(solc_metadata_str) => {
                                solc_metadata_str.as_str()
                            }
                            serde_json::Value::Object(metadata_object) => {
                                let solc_metadata_value = metadata_object
                                    .get("solc_metadata")
                                    .context("Contract doesn't have a 'solc_metadata' field")?;
                                solc_metadata_value
                                    .as_str()
                                    .context("The 'solc_metadata' field is not a string")?
                            }
                            serde_json::Value::Null
                            | serde_json::Value::Bool(_)
                            | serde_json::Value::Number(_)
                            | serde_json::Value::Array(_) => {
                                anyhow::bail!("Unsupported type of metadata {metadata:?}")
                            }
                        };
                        let solc_metadata = serde_json::from_str::<serde_json::Value>(
                            solc_metadata_str,
                        )
                        .context(
                            "Failed to deserialize the solc_metadata as a serde_json generic value",
                        )?;
                        let output_value = solc_metadata
                            .get("output")
                            .context("solc_metadata doesn't have an output field")?;
                        let abi_value = output_value
                            .get("abi")
                            .context("solc_metadata output doesn't contain an abi field")?;
                        serde_json::from_value::<JsonAbi>(abi_value.clone())
                            .context("ABI found in solc_metadata output is not valid ABI")?
                    };
                    map.insert(contract_name, (bytecode.object, abi));
                }
            }

            Ok(compiler_output)
        })
    }

    fn supports_mode(
        &self,
        optimize_setting: ModeOptimizerSetting,
        pipeline: ModePipeline,
    ) -> bool {
        pipeline == ModePipeline::ViaYulIR
            && SolidityCompiler::supports_mode(&self.0.solc, optimize_setting, pipeline)
    }
}
