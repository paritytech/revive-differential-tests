//! Implements the [SolidityCompiler] trait with solc for
//! compiling contracts to EVM bytecode.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{CompilerInput, CompilerOutput, SolidityCompiler};

pub struct Solc {
    solc_path: PathBuf,
}

impl SolidityCompiler for Solc {
    type Options = ();

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

        let output = child.wait_with_output()?.stdout;
        Ok(CompilerOutput {
            input,
            output: serde_json::from_slice(&output)?,
        })
    }

    fn new(solc_path: PathBuf) -> Self {
        Self { solc_path }
    }
}
