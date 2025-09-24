use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

/// A wrapper around processes which allows for their stdout and stderr to be logged and flushed
/// when the process is dropped.
#[derive(Debug)]
pub struct Process {
    /// The handle of the child process.
    child: Child,

    /// The file that stdout is being logged to.
    stdout_logs_file: File,

    /// The file that stderr is being logged to.
    stderr_logs_file: File,
}

impl Process {
    pub fn new(
        log_file_prefix: impl Into<Option<&'static str>>,
        logs_directory: impl AsRef<Path>,
        binary_path: impl AsRef<Path>,
        command_building_callback: impl FnOnce(&mut Command, File, File),
        process_readiness_wait_behavior: ProcessReadinessWaitBehavior,
    ) -> Result<Self> {
        let log_file_prefix = log_file_prefix.into();

        let (stdout_file_name, stderr_file_name) = match log_file_prefix {
            Some(prefix) => (
                format!("{prefix}_stdout.log"),
                format!("{prefix}_stderr.log"),
            ),
            None => ("stdout.log".to_string(), "stderr.log".to_string()),
        };

        let stdout_logs_file_path = logs_directory.as_ref().join(stdout_file_name);
        let stderr_logs_file_path = logs_directory.as_ref().join(stderr_file_name);

        let stdout_logs_file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(stdout_logs_file_path.as_path())
            .context("Failed to open the stdout logs file")?;
        let stderr_logs_file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(stderr_logs_file_path.as_path())
            .context("Failed to open the stderr logs file")?;

        let mut command = {
            let stdout_logs_file = stdout_logs_file
                .try_clone()
                .context("Failed to clone the stdout logs file")?;
            let stderr_logs_file = stderr_logs_file
                .try_clone()
                .context("Failed to clone the stderr logs file")?;

            let mut command = Command::new(binary_path.as_ref());
            command_building_callback(&mut command, stdout_logs_file, stderr_logs_file);
            command
        };
        let child = command
            .spawn()
            .context("Failed to spawn the built command")?;

        match process_readiness_wait_behavior {
            ProcessReadinessWaitBehavior::NoStartupWait => {}
            ProcessReadinessWaitBehavior::WaitDuration(duration) => std::thread::sleep(duration),
            ProcessReadinessWaitBehavior::TimeBoundedWaitFunction {
                max_wait_duration,
                mut check_function,
            } => {
                let spawn_time = Instant::now();

                let stdout_logs_file = OpenOptions::new()
                    .read(true)
                    .open(stdout_logs_file_path)
                    .context("Failed to open the stdout logs file")?;
                let stderr_logs_file = OpenOptions::new()
                    .read(true)
                    .open(stderr_logs_file_path)
                    .context("Failed to open the stderr logs file")?;

                let mut stdout_lines = BufReader::new(stdout_logs_file).lines();
                let mut stderr_lines = BufReader::new(stderr_logs_file).lines();

                let mut stdout = String::new();
                let mut stderr = String::new();

                loop {
                    let stdout_line = stdout_lines.next().and_then(Result::ok);
                    let stderr_line = stderr_lines.next().and_then(Result::ok);

                    if let Some(stdout_line) = stdout_line.as_ref() {
                        stdout.push_str(stdout_line);
                        stdout.push('\n');
                    }
                    if let Some(stderr_line) = stderr_line.as_ref() {
                        stderr.push_str(stderr_line);
                        stdout.push('\n');
                    }

                    let check_result =
                        check_function(stdout_line.as_deref(), stderr_line.as_deref())
                            .context("Failed to wait for the process to be ready")?;

                    if check_result {
                        break;
                    }

                    if Instant::now().duration_since(spawn_time) > max_wait_duration {
                        bail!(
                            "Waited for the process to start but it failed to start in time. stderr {stderr} - stdout {stdout}"
                        )
                    }
                }
            }
        }

        Ok(Self {
            child,
            stdout_logs_file,
            stderr_logs_file,
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.child.kill().expect("Failed to kill the process");
        self.stdout_logs_file
            .flush()
            .expect("Failed to flush the stdout logs file");
        self.stderr_logs_file
            .flush()
            .expect("Failed to flush the stderr logs file");
    }
}

pub enum ProcessReadinessWaitBehavior {
    /// The process does not require any kind of wait after it's been spawned and can be used
    /// straight away.
    NoStartupWait,

    /// The process does require some amount of wait duration after it's been started.
    WaitDuration(Duration),

    /// The process requires a time bounded wait function which is a function of the lines that
    /// appear in the log files.
    TimeBoundedWaitFunction {
        /// The maximum amount of time to wait for the check function to return true.
        max_wait_duration: Duration,

        /// The function to use to check if the process spawned is ready to use or not. This
        /// function should return the following in the following cases:
        ///
        /// - `Ok(true)`: Returned when the condition the process is waiting for has been fulfilled
        ///   and the wait is completed.
        /// - `Ok(false)`: The process is not ready yet but it might be ready in the future.
        /// - `Err`: The process is not ready yet and will not be ready in the future as it appears
        ///   that it has encountered an error when it was being spawned.
        ///
        /// The first argument is a line from stdout and the second argument is a line from stderr.
        #[allow(clippy::type_complexity)]
        check_function: Box<dyn FnMut(Option<&str>, Option<&str>) -> anyhow::Result<bool>>,
    },
}
