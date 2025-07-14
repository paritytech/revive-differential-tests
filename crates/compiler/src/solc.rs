//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_solc_binaries::download_solc;

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
        let mut child = Command::new(&self.solc_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--standard-json")
            .spawn()?;

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

        tracing::debug!(
            output = %String::from_utf8_lossy(&output.stdout).to_string(),
            "Compiled successfully"
        );

        Ok(CompilerOutput {
            input,
            output: serde_json::from_slice(&output.stdout)?,
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
