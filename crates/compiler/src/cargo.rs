//! Compiles Rust contracts with Cargo and links them into PolkaVM programs.

use crate::internal_prelude::*;

/// A Cargo compiler that produces pallet-revive PolkaVM programs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CargoCompiler(Arc<CargoCompilerInner>);

/// The resolved Cargo toolchain and output configuration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CargoCompilerInner {
    /// The resolved Cargo executable.
    cargo_path: PathBuf,
    /// The toolchain argument passed before every Cargo subcommand.
    toolchain: String,
    /// The Cargo toolchain release.
    version: Version,
    /// The inputs that distinguish artifacts produced by this compiler.
    fingerprint: String,
    /// The shared root for Cargo build artifacts.
    target_directory: PathBuf,
    /// The canonical target specification in `polkavm-linker`'s OS cache.
    target_json_path: PathBuf,
}

impl CargoCompiler {
    /// Resolves the configured Cargo toolchain and its PolkaVM target.
    pub fn new(
        configuration: CargoConfiguration,
        working_directory: WorkingDirectoryConfiguration,
    ) -> StaticFuture<Result<Self>> {
        Box::pin(async move {
            /// This is a cache of all of the Cargo compiler objects.
            static COMPILERS_CACHE: LazyLock<DashMap<CargoCompilerInner, CargoCompiler>> =
                LazyLock::new(Default::default);

            let cargo_path = resolve_executable_path(&configuration.command)
                .context("Failed to resolve the Cargo command")?;
            let version_output = AsyncCommand::new(&cargo_path)
                .arg(&configuration.toolchain)
                .arg("-vV")
                .output()
                .await
                .context("Failed to execute the Cargo version command")?;
            anyhow::ensure!(
                version_output.status.success(),
                "Cargo version command failed: {}",
                String::from_utf8_lossy(&version_output.stderr)
            );
            let version = std::str::from_utf8(&version_output.stdout)
                .context("Cargo version output is not UTF-8")?
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("cargo "))
                .and_then(|line| line.split_ascii_whitespace().next())
                .context("Cargo version output does not contain a release")
                .and_then(|version| {
                    Version::parse(version).context("Failed to parse the Cargo release")
                })?;
            let target_json_path =
                polkavm_linker::target_json_path(polkavm_linker::TargetJsonArgs::default())
                    .map_err(anyhow::Error::msg)
                    .context("Failed to create the PolkaVM Rust target specification")?;

            let mut fingerprint = Sha256::new();
            fingerprint.update(
                sha256_file_hex(&cargo_path)
                    .await
                    .context("Failed to fingerprint the Cargo executable")?,
            );
            fingerprint.update(&configuration.toolchain);
            fingerprint.update(&version_output.stdout);
            fingerprint.update(
                sha256_file_hex(&target_json_path)
                    .await
                    .context("Failed to fingerprint the PolkaVM target specification")?,
            );

            let inner = CargoCompilerInner {
                cargo_path,
                toolchain: configuration.toolchain,
                version,
                fingerprint: hex::encode(fingerprint.finalize()),
                target_directory: working_directory
                    .working_directory
                    .as_path()
                    .join("cargo-contracts"),
                target_json_path,
            };
            Ok(COMPILERS_CACHE
                .entry(inner.clone())
                .or_insert_with(|| {
                    info!(
                        cargo_path = %inner.cargo_path.display(),
                        cargo_toolchain = %inner.toolchain,
                        cargo_version = %inner.version,
                        cargo_target_json_path = %inner.target_json_path.display(),
                        "Created a Cargo contract compiler"
                    );
                    Self(Arc::new(inner))
                })
                .clone())
        })
    }

    /// Reads Cargo package and target metadata for one Rust contract project.
    async fn package(&self, manifest_path: &Path) -> Result<CargoContractPackage> {
        let output = AsyncCommand::new(&self.0.cargo_path)
            .current_dir(
                manifest_path
                    .parent()
                    .context("The Cargo manifest has no parent directory")?,
            )
            .arg(&self.0.toolchain)
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .arg("--no-deps")
            .arg("--manifest-path")
            .arg(manifest_path)
            .output()
            .await
            .context("Failed to execute Cargo metadata")?;
        anyhow::ensure!(
            output.status.success(),
            "Cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let metadata =
            serde_json::from_slice::<Metadata>(&output.stdout).context("Invalid Cargo metadata")?;
        CargoContractPackage::try_from_metadata(metadata)
    }

    /// Compiles all binary targets in one Rust contract package.
    async fn compile(&self, manifest_path: &Path) -> Result<()> {
        const IMMEDIATE_ABORT_VERSION: Version = Version::new(1, 92, 0);
        const JSON_TARGET_SPEC_VERSION: Version = Version::new(1, 95, 0);
        const RUSTFLAGS: &str = r#"["-Dwarnings"]"#;
        const IMMEDIATE_ABORT_RUSTFLAGS: &str =
            r#"["-Dwarnings", "-Zunstable-options", "-Cpanic=immediate-abort"]"#;

        let supports_immediate_abort =
            is_gte_major_minor_patch(self.version(), &IMMEDIATE_ABORT_VERSION);
        let supports_json_target_spec =
            is_gte_major_minor_patch(self.version(), &JSON_TARGET_SPEC_VERSION);
        let rustflags = if supports_immediate_abort {
            IMMEDIATE_ABORT_RUSTFLAGS
        } else {
            RUSTFLAGS
        };
        let target_name = self
            .0
            .target_json_path
            .file_stem()
            .and_then(|target_name| target_name.to_str())
            .context("PolkaVM target path has no UTF-8 file name")?;
        let rustflags = format!("target.{target_name}.rustflags={rustflags}");
        let mut command = AsyncCommand::new(&self.0.cargo_path);
        command
            .current_dir(
                manifest_path
                    .parent()
                    .context("The Cargo manifest has no parent directory")?,
            )
            .arg(&self.0.toolchain)
            .arg("--config")
            .arg(rustflags)
            .env("RUSTC_BOOTSTRAP", "1")
            .arg("build")
            .arg("--release")
            .arg("--bins")
            .arg("-Zbuild-std=core")
            .arg("--manifest-path")
            .arg(manifest_path)
            .arg("--target-dir")
            .arg(&self.0.target_directory)
            .arg("--target")
            .arg(&self.0.target_json_path);
        if supports_json_target_spec {
            command.arg("-Zjson-target-spec");
        }
        if !supports_immediate_abort {
            command.arg("-Zbuild-std-features=panic_immediate_abort");
        }

        let output = command
            .output()
            .await
            .context("Failed to execute the Cargo contract build")?;
        anyhow::ensure!(
            output.status.success(),
            "Cargo contract build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    /// Links every compiled ELF into a pallet-revive PolkaVM program.
    async fn link(&self, package: CargoContractPackage) -> Result<CompilerOutput> {
        let artifact_directory = self
            .0
            .target_json_path
            .file_stem()
            .map(|target_name| self.0.target_directory.join(target_name).join("release"))
            .context("PolkaVM target path has no file name")?;
        tokio::task::spawn_blocking(move || {
            let mut output = CompilerOutput::default();

            for target in package.targets {
                let elf_path = artifact_directory.join(&target.name);
                let elf = std::fs::read(&elf_path).with_context(|| {
                    format!("Failed to read Rust contract ELF `{}`", elf_path.display())
                })?;
                let bytecode = polkavm_linker::program_from_elf(
                    polkavm_linker::Config::default(),
                    polkavm_linker::TargetInstructionSet::ReviveV1,
                    &elf,
                )
                .with_context(|| format!("Failed to link Rust contract `{}`", target.name))?;
                let source_path =
                    target
                        .src_path
                        .as_std_path()
                        .canonicalize()
                        .with_context(|| {
                            format!("Failed to canonicalize Rust source `{}`", target.src_path)
                        })?;
                output.contracts.entry(source_path).or_default().insert(
                    package.name.clone(),
                    (hex::encode(bytecode), package.abi.clone()),
                );
            }

            Ok(output)
        })
        .await
        .context("Rust contract linking task failed")?
    }
}

impl ContractCompiler for CargoCompiler {
    fn version(&self) -> &Version {
        &self.0.version
    }

    fn frontend_version(&self) -> &Version {
        self.version()
    }

    fn path(&self) -> &Path {
        &self.0.cargo_path
    }

    fn fingerprint(&self) -> &str {
        &self.0.fingerprint
    }

    fn build(&self, input: CompilerInput) -> StaticFuture<Result<CompilerOutput>> {
        let this = self.clone();
        Box::pin(async move {
            if input.pipeline.is_some() || input.optimization.is_some() {
                warn!("Cargo ignores compiler modes; configure Rust optimization in Cargo.toml");
            }
            let metadata_file_path = input
                .metadata_file_path
                .context("Cargo compilation requires a metadata file path")?
                .canonicalize()
                .context("Failed to canonicalize the metadata file path")?;
            let manifest_path = metadata_file_path
                .parent()
                .context("The metadata file has no parent directory")?
                .join("Cargo.toml");
            anyhow::ensure!(
                manifest_path.is_file(),
                "Rust contract manifest does not exist at `{}`",
                manifest_path.display()
            );
            let manifest_path = manifest_path.canonicalize().with_context(|| {
                format!(
                    "Failed to canonicalize Rust contract manifest `{}`",
                    manifest_path.display()
                )
            })?;
            this.compile(&manifest_path).await?;
            let package = this.package(&manifest_path).await?;
            this.link(package).await
        })
    }

    fn supports_mode(&self, _: &Mode) -> bool {
        true
    }
}

/// The ABI and binary targets declared by one Rust contract package.
#[derive(Clone, Debug)]
struct CargoContractPackage {
    /// The contract name exposed to workload metadata.
    name: String,
    /// The ABI shared by the package's contract binaries.
    abi: JsonAbi,
    /// The binary targets that Cargo compiles into contracts.
    targets: Vec<Target>,
}

impl CargoContractPackage {
    /// Extracts contract information from Cargo's package metadata.
    fn try_from_metadata(metadata: Metadata) -> Result<Self> {
        let package = metadata
            .root_package()
            .context("Cargo metadata does not contain a root package")?;
        let package_metadata = package
            .metadata
            .get("retester")
            .context("Cargo.toml is missing `[package.metadata.retester]`")?;
        let package_metadata =
            serde_json::from_value::<RetesterPackageMetadata>(package_metadata.clone())
                .context("Invalid `[package.metadata.retester]` configuration")?;
        let targets = package
            .targets
            .iter()
            .filter(|target| target.kind.contains(&TargetKind::Bin))
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !targets.is_empty(),
            "Rust contract package contains no binary targets"
        );

        Ok(Self {
            name: package.name.clone().into_inner(),
            abi: package_metadata.abi,
            targets,
        })
    }
}

/// Retester-specific fields under Cargo's package metadata.
#[derive(Clone, Debug, Deserialize)]
struct RetesterPackageMetadata {
    /// The manually maintained JSON ABI for the Rust contract package.
    abi: JsonAbi,
}
