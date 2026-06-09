use crate::internal_prelude::*;

#[derive(Debug)]
pub struct NodeProcess {
    child: Child,
}

impl NodeProcess {
    pub fn new(child: Child) -> Self {
        Self { child }
    }

    pub fn builder(binary: impl AsRef<OsStr>) -> NodeProcessBuilder<'static, 'static> {
        NodeProcessBuilder::new(binary)
    }

    #[cfg(unix)]
    fn graceful_shutdown(&self) -> Result<()> {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .context("Failed to gracefully shutdown")?;
        if !status.success() {
            bail!(
                "Attempted graceful shutdown but failed. Status code: {:?}.",
                status.code()
            )
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn graceful_shutdown(&mut self) -> Result<()> {
        self.child.kill().context("Failed to kill the process")
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        if let Err(err) = self.graceful_shutdown() {
            self.child
                .kill()
                .context(err)
                .expect("Failed to force kill the process");
        }

        let timeout = Duration::from_secs(90);
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self
                .child
                .try_wait()
                .expect("Failed to wait for the child to finish")
                .is_some()
            {
                return;
            } else {
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        self.child.kill().expect("Failed to force kill the child");
        self.child
            .wait()
            .expect("Failed to wait for the child to be killed");
    }
}

pub struct NodeProcessBuilder<'a, 'b> {
    command: Command,
    output: Option<(PathBuf, String)>,
    wait_for_startup_sentinel_information: Option<WaitForStartupSentinel<'a, 'b>>,
}

impl NodeProcessBuilder<'static, 'static> {
    pub fn new(binary: impl AsRef<OsStr>) -> Self {
        Self {
            command: Command::new(binary),
            output: Default::default(),
            wait_for_startup_sentinel_information: None,
        }
    }
}

impl<'a, 'b> NodeProcessBuilder<'a, 'b> {
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    pub fn log(mut self, name: impl AsRef<str>, directory: impl AsRef<Path>) -> Self {
        self.output = Some((directory.as_ref().to_owned(), name.as_ref().to_owned()));
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn wait_for_startup<'c, 'd>(
        self,
        sentinel_information: WaitForStartupSentinel<'c, 'd>,
    ) -> NodeProcessBuilder<'c, 'd> {
        NodeProcessBuilder {
            command: self.command,
            output: self.output,
            wait_for_startup_sentinel_information: Some(sentinel_information),
        }
    }

    pub fn with_common_substrate_node_args(
        self,
        chainspec_path: impl AsRef<Path>,
        data_directory: impl AsRef<Path>,
        node_port: AllocatedPort,
        logging_level: impl AsRef<OsStr>,
    ) -> Self {
        [
            "--rpc-max-request-size",
            "--rpc-max-response-size",
            "--rpc-max-connections",
            "--pool-limit",
            "--pool-kbytes",
            "--state-pruning",
            "--rpc-max-subscriptions-per-connection",
        ]
        .into_iter()
        .fold(self, |this, key| this.arg(key).arg(u32::MAX.to_string()))
        .arg("--log")
        .arg(logging_level.as_ref())
        .arg("--rpc-port")
        .arg(node_port.value().to_string())
        .arg("--rpc-methods")
        .arg("unsafe")
        .arg("--rpc-cors")
        .arg("all")
        .arg("--no-prometheus")
        .arg("--base-path")
        .arg(data_directory.as_ref())
        .arg("--chain")
        .arg(chainspec_path.as_ref())
        .env("RUST_LOG", logging_level)
    }

    pub fn build(mut self) -> Result<NodeProcess> {
        let (child, readers) = if let Some((path, name)) = self.output {
            let logs_files = StdoutStdErr::try_from_fn(|postfix| {
                create_file(path.as_path(), name.as_str(), postfix, true)
            })
            .context("Failed to create r/w log files")?;
            let read_files = StdoutStdErr::try_from_fn(|postfix| {
                create_file(path.as_path(), name.as_str(), postfix, false)
            })
            .context("Failed to create r/w log files")?;

            let child = self
                .command
                .stdout(logs_files.stdout)
                .stderr(logs_files.stderr)
                .spawn()
                .context("Failed to spawn process")?;

            let readers = read_files.map(|f| Box::new(f) as Box<dyn Read>);

            (child, readers)
        } else {
            let mut child = self
                .command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to spawn process")?;

            let readers = StdoutStdErr::<Box<dyn Read>> {
                stdout: Box::new(child.stdout.take().expect("qed; piped above")),
                stderr: Box::new(child.stderr.take().expect("qed; piped above")),
            };

            (child, readers)
        };

        if let Some(wait_for_startup_sentinel) = self.wait_for_startup_sentinel_information {
            let mut readers = readers.map(BufReader::new).map(BufReader::lines);

            // TODO(async): We need to honor the timeout which we can easily do once we refactor
            // this to be async through tokio's timeout capabilities.
            'check: loop {
                let next_lines = readers.map_ref_mut(|lines| lines.next().and_then(Result::ok));
                for line in [next_lines.stdout, next_lines.stderr].into_iter().flatten() {
                    match wait_for_startup_sentinel.check(line) {
                        ControlFlow::Continue(_) => {}
                        ControlFlow::Break(Ok(())) => break 'check,
                        ControlFlow::Break(err @ Err(..)) => {
                            err.context("Failed to startup process")?;
                        }
                    }
                }
            }
        }

        Ok(NodeProcess::new(child))
    }
}

pub struct WaitForStartupSentinel<'a, 'b> {
    timeout: Duration,
    successful_startup_if_encountered: Cow<'a, str>,
    failed_startup_if_encountered: Option<Cow<'b, str>>,
}

impl<'a> WaitForStartupSentinel<'a, 'static> {
    pub fn new(
        timeout: Duration,
        successful_startup_if_encountered: impl Into<Cow<'a, str>>,
    ) -> Self {
        Self {
            timeout,
            successful_startup_if_encountered: successful_startup_if_encountered.into(),
            failed_startup_if_encountered: None,
        }
    }
}

impl<'a, 'b> WaitForStartupSentinel<'a, 'b> {
    pub fn with_failed_startup_if_encountered<'c>(
        self,
        failed_startup_if_encountered: impl Into<Cow<'c, str>>,
    ) -> WaitForStartupSentinel<'a, 'c> {
        WaitForStartupSentinel {
            timeout: self.timeout,
            successful_startup_if_encountered: self.successful_startup_if_encountered,
            failed_startup_if_encountered: Some(failed_startup_if_encountered.into()),
        }
    }

    pub fn check(&self, line: impl AsRef<str>) -> ControlFlow<Result<()>, ()> {
        let line = line.as_ref();
        if line.contains(self.successful_startup_if_encountered.as_ref()) {
            ControlFlow::Break(Ok(()))
        } else if let Some(ref failed_startup_if_encountered) = self.failed_startup_if_encountered
            && line.contains(failed_startup_if_encountered.as_ref())
        {
            ControlFlow::Break(Err(anyhow::anyhow!(
                "Encountered failure sentinel: {failed_startup_if_encountered}"
            )))
        } else {
            ControlFlow::Continue(())
        }
    }
}

struct StdoutStdErr<T> {
    pub stdout: T,
    pub stderr: T,
}

impl<T> StdoutStdErr<T> {
    pub fn try_from_fn<E>(mut func: impl FnMut(&str) -> Result<T, E>) -> Result<Self, E> {
        Ok(Self {
            stdout: func("stdout")?,
            stderr: func("stderr")?,
        })
    }

    pub fn map<O>(self, mut func: impl FnMut(T) -> O) -> StdoutStdErr<O> {
        StdoutStdErr {
            stdout: func(self.stdout),
            stderr: func(self.stderr),
        }
    }

    pub fn map_ref_mut<O>(&mut self, mut func: impl FnMut(&mut T) -> O) -> StdoutStdErr<O> {
        StdoutStdErr {
            stdout: func(&mut self.stdout),
            stderr: func(&mut self.stderr),
        }
    }
}

fn create_file(
    path: impl AsRef<Path>,
    name: impl AsRef<str>,
    postfix: impl AsRef<str>,
    is_read_write: bool,
) -> Result<File> {
    let file_name = format!("{}_{}.log", name.as_ref(), postfix.as_ref());
    let file_path = path.as_ref().join(file_name);

    let mut open_options = OpenOptions::new();
    let open_options = if is_read_write {
        open_options
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
    } else {
        open_options.read(true)
    };

    open_options.open(file_path).context("Failed to open file")
}
