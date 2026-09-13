use crate::internal_prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltRuntime {
    pub commit: Oid,
    pub wasm_path: PathBuf,
    pub wasm: Vec<u8>,
}

pub async fn build_runtime(branch: impl AsRef<str>) -> Result<BuiltRuntime> {
    let checkout_dir = SDK_DIRECTORY.as_path();
    let target_dir = RUNTIME_TARGET_DIRECTORY.as_path();
    fs::create_dir_all(target_dir)
        .await
        .with_context(|| format!("Failed to create runtime target {}", target_dir.display()))?;

    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(BUILD_LOCK_PATH.as_path())
        .await
        .context("Failed to open runtime build lock")?
        .into_std()
        .await;
    let _build_lock = spawn_blocking(move || -> Result<_> {
        lock.lock().context("Failed to lock runtime build")?;
        Ok(lock)
    })
    .await
    .context("Runtime build lock task failed")??;

    let checkout_branch = branch.as_ref().to_owned();
    let commit = spawn_blocking(move || checkout(&checkout_branch))
        .await
        .context("SDK checkout task failed")??;

    let status = Command::new("cargo")
        .args([
            "+1.93.0",
            "build",
            "--locked",
            "--profile",
            "production",
            "--package",
            "asset-hub-westend-runtime",
            "--target-dir",
        ])
        .arg(target_dir)
        .current_dir(checkout_dir)
        .env(
            "RUSTFLAGS",
            "-Awarnings --cfg revive_jit --cfg revive_debug",
        )
        .env(
            "WASM_BUILD_RUSTFLAGS",
            "-Clink-arg=--allow-undefined --cfg revive_debug",
        )
        .env("WASM_BUILD_TYPE", "production")
        .env("WASM_BUILD_TOOLCHAIN", "1.93.0")
        .env("WASM_BUILD_WORKSPACE_HINT", checkout_dir)
        .env("SUBSTRATE_RUNTIME_TARGET", "wasm")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("SKIP_WASM_BUILD")
        .env_remove("SKIP_ASSET_HUB_WESTEND_RUNTIME_WASM_BUILD")
        .env_remove("DOCS_RS")
        .env_remove("WASM_BUILD_STD")
        .env_remove("WASM_BUILD_CARGO_ARGS")
        .env_remove("WASM_TARGET_DIRECTORY")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .status()
        .with_timed(|duration| {
            debug!(
                duration_ms = duration.as_millis(),
                "Westend runtime build finished"
            );
        })
        .await
        .context("Failed to start Cargo")?;
    ensure!(status.success(), "Runtime build failed: {status}");

    let wasm_path = target_dir
        .join("production/wbuild/asset-hub-westend-runtime/asset_hub_westend_runtime.compact.wasm");
    let wasm = fs::read(&wasm_path)
        .await
        .with_context(|| format!("Failed to read runtime Wasm {}", wasm_path.display()))?;

    info!(
        branch = branch.as_ref(),
        commit = %commit,
        wasm_path = %wasm_path.display(),
        wasm_size_bytes = wasm.len(),
        "Westend runtime has been built"
    );

    Ok(BuiltRuntime {
        commit,
        wasm_path,
        wasm,
    })
}
