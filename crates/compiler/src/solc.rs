//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::{ResolcConfiguration, SolcConfiguration, WorkingDirectoryConfiguration};
use revive_dt_solc_binaries::download_solc;

use crate::{
    CompilerInput, CompilerOutput, DynSolidityCompiler, ModeOptimizerSetting, ModePipeline,
    SolidityCompiler,
};

use anyhow::{Context as _, Result};
use foundry_compilers_artifacts::{
    output_selection::{
        BytecodeOutputSelection, ContractOutputSelection, EvmOutputSelection, OutputSelection,
    },
    solc::CompilerOutput as SolcOutput,
    solc::*,
};
use semver::Version;
use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Solc(Arc<SolcInner>);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SolcInner {
    /// The path of the solidity compiler executable that this object uses.
    solc_path: PathBuf,
    /// The version of the solidity compiler executable that this object uses.
    solc_version: Version,
}

impl DynSolidityCompiler for Solc {
    fn version(&self) -> &Version {
        SolidityCompiler::version(self)
    }

    fn path(&self) -> &std::path::Path {
        SolidityCompiler::path(self)
    }

    fn build(
        &self,
        input: CompilerInput,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<CompilerOutput>> + '_>> {
        Box::pin(SolidityCompiler::build(self, input))
    }

    fn supports_mode(
        &self,
        optimizer_setting: ModeOptimizerSetting,
        pipeline: ModePipeline,
    ) -> bool {
        SolidityCompiler::supports_mode(self, optimizer_setting, pipeline)
    }
}

impl SolidityCompiler for Solc {
    async fn new(
        context: impl AsRef<SolcConfiguration>
        + AsRef<ResolcConfiguration>
        + AsRef<WorkingDirectoryConfiguration>,
        version: impl Into<Option<VersionOrRequirement>>,
    ) -> Result<Self> {
        // This is a cache for the compiler objects so that whenever the same compiler version is
        // requested the same object is returned. We do this as we do not want to keep cloning the
        // compiler around.
        static COMPILERS_CACHE: LazyLock<DashMap<(PathBuf, Version), Solc>> =
            LazyLock::new(Default::default);

        let working_directory_configuration =
            AsRef::<WorkingDirectoryConfiguration>::as_ref(&context);
        let solc_configuration = AsRef::<SolcConfiguration>::as_ref(&context);

        // We attempt to download the solc binary. Note the following: this call does the version
        // resolution for us. Therefore, even if the download didn't proceed, this function will
        // resolve the version requirement into a canonical version of the compiler. It's then up
        // to us to either use the provided path or not.
        let version = version
            .into()
            .unwrap_or_else(|| solc_configuration.version.clone().into());
        let (version, path) =
            download_solc(working_directory_configuration.as_path(), version, false)
                .await
                .context("Failed to download/get path to solc binary")?;

        Ok(COMPILERS_CACHE
            .entry((path.clone(), version.clone()))
            .or_insert_with(|| {
                Self(Arc::new(SolcInner {
                    solc_path: path,
                    solc_version: version,
                }))
            })
            .clone())
    }

    fn version(&self) -> &Version {
        &self.0.solc_version
    }

    fn path(&self) -> &std::path::Path {
        &self.0.solc_path
    }

    #[tracing::instrument(level = "debug", ret)]
    async fn build(
        &self,
        CompilerInput {
            pipeline,
            optimization,
            evm_version,
            allow_paths,
            base_path,
            sources,
            libraries,
            revert_string_handling,
        }: CompilerInput,
    ) -> Result<CompilerOutput> {
        // Be careful to entirely omit the viaIR field if the compiler does not support it,
        // as it will error if you provide fields it does not know about. Because
        // `supports_mode` is called prior to instantiating a compiler, we should never
        // ask for something which is invalid.
        let via_ir = match (pipeline, self.compiler_supports_yul()) {
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

        let path = &self.0.solc_path;
        let mut command = AsyncCommand::new(path);
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
            .with_context(|| format!("Failed to spawn solc at {}", path.display()))?;

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

        let mut compiler_output = CompilerOutput::default();
        for (contract_path, contracts) in parsed.contracts {
            let map = compiler_output
                .contracts
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

        Ok(compiler_output)
    }

    fn supports_mode(
        &self,
        _optimize_setting: ModeOptimizerSetting,
        pipeline: ModePipeline,
    ) -> bool {
        // solc 0.8.13 and above supports --via-ir, and less than that does not. Thus, we support mode E
        // (ie no Yul IR) in either case, but only support Y (via Yul IR) if the compiler is new enough.
        pipeline == ModePipeline::ViaEVMAssembly
            || (pipeline == ModePipeline::ViaYulIR && self.compiler_supports_yul())
    }
}

impl Solc {
    fn compiler_supports_yul(&self) -> bool {
        const SOLC_VERSION_SUPPORTING_VIA_YUL_IR: Version = Version::new(0, 8, 13);
        DynSolidityCompiler::version(self) >= &SOLC_VERSION_SUPPORTING_VIA_YUL_IR
    }
}
