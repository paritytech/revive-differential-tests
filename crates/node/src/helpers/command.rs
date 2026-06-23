use crate::internal_prelude::*;

pub trait CommandExt {
    fn run_and_get_output(&mut self) -> Result<CommandOutput>;
}

impl CommandExt for Command {
    fn run_and_get_output(&mut self) -> Result<CommandOutput> {
        let output = self
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed while waiting for command")?;

        let cmd_output = CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };

        if !output.status.success() {
            bail!("Processes returned a non-success status code. Output = {cmd_output:?}",)
        }

        Ok(cmd_output)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}
