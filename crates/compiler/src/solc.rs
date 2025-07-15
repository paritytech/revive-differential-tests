//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_solc_binaries::download_solc;
use revive_solc_json_interface::SolcStandardJsonOutput;

pub struct Solc {
    solc_path: PathBuf,
}

impl SolidityCompiler for Solc {
    type Options = ();

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
        version: semver::Version,
    ) -> anyhow::Result<PathBuf> {
        let path = download_solc(config.directory(), version, config.wasm)?;
        Ok(path)
    }
}
