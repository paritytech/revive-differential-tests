use anyhow::Result;
use std::{
    process::{Command, Stdio},
    {path::Path, time::Duration},
};

use crate::helpers::{Process, ProcessReadinessWaitBehavior};

const PROXY_LOG_ENV: &str = "info,eth-rpc=debug";

pub fn spawn_eth_rpc_process(
    logs_directory: &Path,
    eth_proxy_binary: &Path,
    node_rpc_url: &str,
    eth_rpc_port: u16,
    extra_args: &[&str],
    ready_marker: &str,
) -> anyhow::Result<Process> {
    let ready_marker = ready_marker.to_owned();
    Process::new(
        "proxy",
        logs_directory,
        eth_proxy_binary,
        |command, stdout_file, stderr_file| {
            command
                .arg("--node-rpc-url")
                .arg(node_rpc_url)
                .arg("--rpc-cors")
                .arg("all")
                .arg("--rpc-max-connections")
                .arg(u32::MAX.to_string())
                .arg("--rpc-port")
                .arg(eth_rpc_port.to_string())
                .env("RUST_LOG", PROXY_LOG_ENV);
            for arg in extra_args {
                command.arg(arg);
            }
            command.stdout(stdout_file).stderr(stderr_file);
        },
        ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
            max_wait_duration: Duration::from_secs(30),
            check_function: Box::new(move |_, stderr_line| match stderr_line {
                Some(line) => Ok(line.contains(&ready_marker)),
                None => Ok(false),
            }),
        },
    )
}

pub fn command_version(command_binary: &Path) -> Result<String> {
    let output = Command::new(command_binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?
        .wait_with_output()?
        .stdout;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}
