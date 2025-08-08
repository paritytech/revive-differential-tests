//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::Arguments;
use revive_dt_solc_binaries::download_solc;

use crate::{CompilerInput, CompilerOutput, ModeOptimizerSetting, ModePipeline, SolidityCompiler};

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
    solc_path: PathBuf,
}

impl SolidityCompiler for Solc {
    type Options = ();

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
        }: CompilerInput,
        _: Self::Options,
    ) -> anyhow::Result<CompilerOutput> {
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
                via_ir: pipeline.map(|p| p.via_yul_ir()),
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
                ..Default::default()
            },
        };

        let mut command = AsyncCommand::new(&self.solc_path);
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
        let mut child = command.spawn()?;

        let stdin = child.stdin.as_mut().expect("should be piped");
        let serialized_input = serde_json::to_vec(&input)?;
        stdin.write_all(&serialized_input).await?;
        let output = child.wait_with_output().await?;

        if !output.status.success() {
            let json_in = serde_json::to_string_pretty(&input)?;
            let message = String::from_utf8_lossy(&output.stderr);
            tracing::error!(
                status = %output.status,
                message = %message,
                json_input = json_in,
                "Compilation using solc failed"
            );
            anyhow::bail!("Compilation failed with an error: {message}");
        }

        let parsed = serde_json::from_slice::<SolcOutput>(&output.stdout).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse resolc JSON output: {e}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })?;

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
                .entry(contract_path.canonicalize()?)
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

    fn new(solc_path: PathBuf) -> Self {
        Self { solc_path }
    }

    async fn get_compiler_executable(
        config: &Arguments,
        version: impl Into<VersionOrRequirement>,
    ) -> anyhow::Result<PathBuf> {
        let path = download_solc(config.directory(), version, config.wasm).await?;
        Ok(path)
    }

    fn version(&self) -> anyhow::Result<semver::Version> {
        // The following is the parsing code for the version from the solc version strings which
        // look like the following:
        // ```
        // solc, the solidity compiler commandline interface
        // Version: 0.8.30+commit.73712a01.Darwin.appleclang
        // ```

        let child = Command::new(self.solc_path.as_path())
            .arg("--version")
            .stdout(Stdio::piped())
            .spawn()?;
        let output = child.wait_with_output()?;
        let output = String::from_utf8_lossy(&output.stdout);
        let version_line = output
            .split("Version: ")
            .nth(1)
            .context("Version parsing failed")?;
        let version_string = version_line
            .split("+")
            .next()
            .context("Version parsing failed")?;

        Version::parse(version_string).map_err(Into::into)
    }

    fn supports_mode(
        compiler_version: &Version,
        _optimize_setting: ModeOptimizerSetting,
        pipeline: ModePipeline,
    ) -> bool {
        // solc 0.8.13 and above supports --via-ir, and less than that does not. Thus, we support mode E
        // (ie no Yul IR) in either case, but only support Y (via Yul IR) if the compiler is new enough.
        pipeline == ModePipeline::E
            || (pipeline == ModePipeline::Y && compiler_version >= &Version::new(0, 8, 13))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn compiler_version_can_be_obtained() {
        // Arrange
        let args = Arguments::default();
        println!("Getting compiler path");
        let path = Solc::get_compiler_executable(&args, Version::new(0, 7, 6))
            .await
            .unwrap();
        println!("Got compiler path");
        let compiler = Solc::new(path);

        // Act
        let version = compiler.version();

        // Assert
        assert_eq!(
            version.expect("Failed to get version"),
            Version::new(0, 7, 6)
        )
    }

    #[tokio::test]
    async fn compiler_version_can_be_obtained1() {
        // Arrange
        let args = Arguments::default();
        println!("Getting compiler path");
        let path = Solc::get_compiler_executable(&args, Version::new(0, 4, 21))
            .await
            .unwrap();
        println!("Got compiler path");
        let compiler = Solc::new(path);

        // Act
        let version = compiler.version();

        // Assert
        assert_eq!(
            version.expect("Failed to get version"),
            Version::new(0, 4, 21)
        )
    }
}
