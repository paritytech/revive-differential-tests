//! Implements the [SolidityCompiler] trait with `resolc` for
//! compiling contracts to PolkaVM (PVM) bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};
use revive_solc_json_interface::SolcStandardJsonOutput;

/// A wrapper around the `resolc` binary, emitting PVM-compatible bytecode.
pub struct Resolv {
    /// Path to the `resolc` executable
    resolc_path: PathBuf,
}

impl SolidityCompiler for Resolv {
    /// No extra compiler options for now.
    type Options = ();

    fn build(
        &self,
        input: CompilerInput<Self::Options>,
    ) -> anyhow::Result<CompilerOutput<Self::Options>> {

        let mut child = Command::new(&self.resolc_path)
            .arg("--standard-json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())   // show errors directly
            .spawn()?;

        serde_json::to_writer(
            child.stdin.as_mut().expect("stdin must be piped"),
            &input.input,
        )?;

        let stdout = child.wait_with_output()?.stdout;

        let output: SolcStandardJsonOutput = serde_json::from_slice(&stdout)?;

        Ok(CompilerOutput { input, output })
    }

    fn new(resolc_path: PathBuf) -> Self {
        Resolv { resolc_path }
    }
}
