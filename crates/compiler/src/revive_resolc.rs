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
pub struct Resolc {
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
}

impl SolidityCompiler for Resolc {
    type Options = Vec<String>;

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

        let parsed: SolcStandardJsonOutput = serde_json::from_slice(&stdout).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse resolc JSON output: {e}\nstderr: {}",
                String::from_utf8_lossy(&stderr)
            )
        })?;

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
