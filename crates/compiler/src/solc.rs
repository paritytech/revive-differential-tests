//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};
use anyhow::Context;
use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::Arguments;
use revive_dt_solc_binaries::download_solc;
use revive_solc_json_interface::SolcStandardJsonOutput;
use semver::Version;

#[derive(Debug)]
pub struct Solc {
    solc_path: PathBuf,
}

impl SolidityCompiler for Solc {
    type Options = ();

    #[tracing::instrument(level = "debug", ret)]
    fn build(
        &self,
        input: CompilerInput<Self::Options>,
    ) -> anyhow::Result<CompilerOutput<Self::Options>> {
        let mut command = Command::new(&self.solc_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--standard-json");

        if let Some(ref base_path) = input.base_path {
            command.arg("--base-path").arg(base_path);
        }
        if !input.allow_paths.is_empty() {
            command.arg("--allow-paths").arg(
                input
                    .allow_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        let mut child = command.spawn()?;

        let stdin = child.stdin.as_mut().expect("should be piped");
        serde_json::to_writer(stdin, &input.input)?;
        let output = child.wait_with_output()?;

        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            tracing::error!("solc failed exit={} stderr={}", output.status, &message);
            return Ok(CompilerOutput {
                input,
                output: Default::default(),
                error: Some(message.into()),
            });
        }

        let parsed =
            serde_json::from_slice::<SolcStandardJsonOutput>(&output.stdout).map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse resolc JSON output: {e}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            })?;

        // Detecting if the compiler output contained errors and reporting them through logs and
        // errors instead of returning the compiler output that might contain errors.
        for error in parsed.errors.iter().flatten() {
            if error.severity == "error" {
                tracing::error!(?error, ?input, "Encountered an error in the compilation");
                anyhow::bail!("Encountered an error in the compilation: {error}")
            }
        }

        tracing::debug!(
            output = %String::from_utf8_lossy(&output.stdout).to_string(),
            "Compiled successfully"
        );

        Ok(CompilerOutput {
            input,
            output: parsed,
            error: None,
        })
    }

    fn new(solc_path: PathBuf) -> Self {
        Self { solc_path }
    }

    fn get_compiler_executable(
        config: &Arguments,
        version: impl Into<VersionOrRequirement>,
    ) -> anyhow::Result<PathBuf> {
        let path = download_solc(config.directory(), version, config.wasm)?;
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
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn compiler_version_can_be_obtained() {
        // Arrange
        let args = Arguments::default();
        let path = Solc::get_compiler_executable(&args, Version::new(0, 7, 6)).unwrap();
        let compiler = Solc::new(path);

        // Act
        let version = compiler.version();

        // Assert
        assert_eq!(
            version.expect("Failed to get version"),
            Version::new(0, 7, 6)
        )
    }
}
