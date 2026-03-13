//! Implements the [SolidityCompiler] trait with `resolc` for
//! compiling contracts to PolkaVM (PVM) bytecode.

use crate::internal_prelude::*;

/// A wrapper around the `resolc` binary, emitting PVM-compatible bytecode.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Resolc(Arc<ResolcInner>);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ResolcInner {
    /// The internal solc compiler that the resolc compiler uses as a compiler frontend.
    solc: Solc,
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
    /// The PVM heap size in bytes.
    pvm_heap_size: u32,
    /// The PVM stack size in bytes.
    pvm_stack_size: u32,
}

impl Resolc {
    pub fn new(
        context: impl HasSolcConfiguration
        + HasResolcConfiguration
        + HasWorkingDirectoryConfiguration
        + Send
        + 'static,
        version: impl Into<Option<VersionOrRequirement>> + Send + 'static,
    ) -> FrameworkFuture<Result<Self>> {
        Box::pin(async move {
            /// This is a cache of all of the resolc compiler objects. Since we do not currently support
            /// multiple resolc compiler versions, so our cache is just keyed by the solc compiler and
            /// its version to the resolc compiler.
            static COMPILERS_CACHE: LazyLock<DashMap<Solc, Resolc>> =
                LazyLock::new(Default::default);

            let resolc_configuration = context.as_resolc_configuration();
            let resolc_path = resolc_configuration.path.clone();
            let pvm_heap_size = resolc_configuration
                .heap_size
                .unwrap_or(PolkaVMDefaultHeapMemorySize);
            let pvm_stack_size = resolc_configuration
                .stack_size
                .unwrap_or(PolkaVMDefaultStackMemorySize);

            let solc = Solc::new(context, version)
                .await
                .context("Failed to create the solc compiler frontend for resolc")?;

            Ok(COMPILERS_CACHE
                .entry(solc.clone())
                .or_insert_with(|| {
                    Self(Arc::new(ResolcInner {
                        solc,
                        resolc_path,
                        pvm_heap_size,
                        pvm_stack_size,
                    }))
                })
                .clone())
        })
    }

    fn polkavm_settings(&self) -> SolcStandardJsonInputSettingsPolkaVM {
        SolcStandardJsonInputSettingsPolkaVM::new(
            Some(SolcStandardJsonInputSettingsPolkaVMMemory::new(
                Some(self.0.pvm_heap_size),
                Some(self.0.pvm_stack_size),
            )),
            false,
        )
    }

    /// Manually inject resolc-specific settings that are marked `skip_serializing`
    /// in the upstream `revive-solc-json-interface` crate.
    fn inject_resolc_specific_settings(input: &SolcStandardJsonInput) -> Result<serde_json::Value> {
        let mut input_value = serde_json::to_value(input)
            .context("Failed to serialize Standard JSON input for resolc")?;
        if let Some(settings) = input_value.get_mut("settings") {
            settings["polkavm"] = serde_json::to_value(input.settings.polkavm).unwrap();
            if let Some(optimizer) = settings.get_mut("optimizer") {
                optimizer["mode"] =
                    serde_json::Value::String(input.settings.optimizer.mode.to_string());
            }
        }
        Ok(input_value)
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

    #[tracing::instrument(level = "debug", skip_all, ret)]
    #[tracing::instrument(
        level = "error",
        skip_all,
        fields(
            resolc_version = %this.version(),
            solc_version = %this.0.solc.version(),
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
    ) -> FrameworkFuture<Result<CompilerOutput>> {
        let this = self.clone();
        Box::pin(async move {
            if !matches!(pipeline, None | Some(ModePipeline::ViaYulIR)) {
                anyhow::bail!(
                    "Resolc only supports the Y (via Yul IR) pipeline, but the provided pipeline is {pipeline:?}"
                );
            }

            let optimize_setting = optimization.unwrap_or_default();

            let input = SolcStandardJsonInput {
                language: SolcStandardJsonInputLanguage::Solidity,
                sources: sources
                    .into_iter()
                    .map(|(path, source)| (path.display().to_string(), source.into()))
                    .collect(),
                settings: SolcStandardJsonInputSettings {
                    evm_version,
                    libraries: SolcStandardJsonInputSettingsLibraries {
                        inner: libraries
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
                    },
                    remappings: BTreeSet::<String>::new(),
                    output_selection:
                        SolcStandardJsonInputSettingsSelection::new_required_for_tests(),
                    via_ir: Some(true),
                    optimizer: SolcStandardJsonInputSettingsOptimizer::new(
                        optimize_setting.solc_optimizer_enabled,
                        optimize_setting.level.to_mode_char(),
                        // Don't disable the optimizer details. `SolcOptimizerDetails::default()` will
                        // return `None` for each optimizer detail, allowing `solc_optimizer_enabled`
                        // to be the determining factor for the default values of solc's optimizer details.
                        SolcOptimizerDetails::default(),
                    ),
                    polkavm: this.polkavm_settings(),
                    metadata: SolcStandardJsonInputSettingsMetadata::default(),
                    detect_missing_libraries: false,
                },
            };
            // Manually inject resolc-specific settings that are marked `skip_serializing` in the upstream crate.
            let std_input_json = Self::inject_resolc_specific_settings(&input)?;

            Span::current().record(
                "json_in",
                display(serde_json::to_string(&std_input_json).unwrap()),
            );

            let path = &this.0.resolc_path;
            let mut command = AsyncCommand::new(path);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .arg("--solc")
                .arg(this.0.solc.path())
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
            let serialized_input = serde_json::to_vec(&std_input_json)
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

            let parsed: SolcStandardJsonOutput = {
                let mut deserializer = serde_json::Deserializer::from_slice(&stdout);
                deserializer.disable_recursion_limit();
                serde::de::Deserialize::deserialize(&mut deserializer)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "failed to parse resolc JSON output: {e}\nstderr: {}",
                            String::from_utf8_lossy(&stderr)
                        )
                    })
                    .context("Failed to parse resolc standard JSON output")?
            };

            tracing::debug!(
                output = %serde_json::to_string(&parsed).unwrap(),
                "Compiled successfully"
            );

            // Detecting if the compiler output contained errors and reporting them through logs and
            // errors instead of returning the compiler output that might contain errors.
            for error in parsed.errors.iter() {
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

            if parsed.contracts.is_empty() {
                anyhow::bail!("Unexpected error - resolc output doesn't have a contracts section");
            }

            let mut compiler_output = CompilerOutput::default();
            for (source_path, contracts) in parsed.contracts.into_iter() {
                let src_for_msg = source_path.clone();
                let source_path = PathBuf::from(source_path)
                    .canonicalize()
                    .with_context(|| format!("Failed to canonicalize path {src_for_msg}"))?;

                let contracts_at_path = compiler_output.contracts.entry(source_path).or_default();
                for (contract_name, contract_information) in contracts.into_iter() {
                    let Some(bytecode) = contract_information
                        .evm
                        .and_then(|evm| evm.bytecode.clone())
                    else {
                        tracing::debug!(
                            "Skipping abstract or interface contract {} - no bytecode",
                            contract_name
                        );
                        continue;
                    };
                    let abi = {
                        let metadata = &contract_information.metadata;
                        if metadata.is_null() {
                            anyhow::bail!("No metadata found for the contract");
                        }

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
                    contracts_at_path.insert(contract_name, (bytecode.object, abi));
                }
            }

            Ok(compiler_output)
        })
    }

    fn supports_mode(&self, mode: &Mode) -> bool {
        mode.pipeline == ModePipeline::ViaYulIR
            && SolidityCompiler::supports_mode(&self.0.solc, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use revive_solc_json_interface::standard_json::input::source::Source as SolcStandardJsonInputSource;

    fn make_solc_input_with_resolc_settings(
        optimizer_mode: char,
        pvm_heap_size: u32,
        pvm_stack_size: u32,
    ) -> SolcStandardJsonInput {
        SolcStandardJsonInput {
            language: SolcStandardJsonInputLanguage::Solidity,
            sources: BTreeMap::from([(
                "test.sol".to_string(),
                SolcStandardJsonInputSource::from("contract T {}".to_string()),
            )]),
            settings: SolcStandardJsonInputSettings {
                evm_version: None,
                libraries: Default::default(),
                remappings: Default::default(),
                output_selection: Default::default(),
                via_ir: Some(true),
                optimizer: SolcStandardJsonInputSettingsOptimizer::new(
                    true,
                    optimizer_mode,
                    SolcOptimizerDetails::default(),
                ),
                polkavm: SolcStandardJsonInputSettingsPolkaVM::new(
                    Some(SolcStandardJsonInputSettingsPolkaVMMemory::new(
                        Some(pvm_heap_size),
                        Some(pvm_stack_size),
                    )),
                    false,
                ),
                metadata: Default::default(),
                detect_missing_libraries: false,
            },
        }
    }

    fn assert_expected_solc_input(
        json_input: &serde_json::Value,
        optimizer_mode: char,
        pvm_heap_size: u32,
        pvm_stack_size: u32,
    ) {
        let optimizer = &json_input["settings"]["optimizer"];
        assert_eq!(
            optimizer["mode"].as_str(),
            Some(optimizer_mode.to_string().as_str())
        );
        assert_eq!(optimizer["enabled"].as_bool(), Some(true));

        let polkavm = &json_input["settings"]["polkavm"];
        assert_eq!(
            polkavm["memoryConfig"]["heapSize"].as_u64(),
            Some(pvm_heap_size as u64)
        );
        assert_eq!(
            polkavm["memoryConfig"]["stackSize"].as_u64(),
            Some(pvm_stack_size as u64)
        );
        assert_eq!(polkavm["debugInformation"].as_bool(), Some(false));

        // Standard fields survive the round-trip.
        assert_eq!(json_input["language"].as_str(), Some("Solidity"));
        assert!(json_input["sources"]["test.sol"].is_object());
        assert_eq!(json_input["settings"]["viaIR"].as_bool(), Some(true));
    }

    #[test]
    fn injects_resolc_specific_settings() {
        let input = make_solc_input_with_resolc_settings('0', 100_000, 50_000);
        let json_input = Resolc::inject_resolc_specific_settings(&input).unwrap();
        assert_expected_solc_input(&json_input, '0', 100_000, 50_000);

        let input = make_solc_input_with_resolc_settings('3', 200_000, 75_000);
        let json_input = Resolc::inject_resolc_specific_settings(&input).unwrap();
        assert_expected_solc_input(&json_input, '3', 200_000, 75_000);

        let input = make_solc_input_with_resolc_settings('z', 300_000, 100_000);
        let json_input = Resolc::inject_resolc_specific_settings(&input).unwrap();
        assert_expected_solc_input(&json_input, 'z', 300_000, 100_000);
    }
}
