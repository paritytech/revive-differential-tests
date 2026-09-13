use crate::internal_prelude::*;

pub(crate) static SDK_DIRECTORY: LazyLock<PathBuf> =
    LazyLock::new(|| RetesterHomeDirectory::get_path().join("polkadot-sdk"));
pub(crate) static RUNTIME_TARGET_DIRECTORY: LazyLock<PathBuf> =
    LazyLock::new(|| RetesterHomeDirectory::get_path().join("runtime_target"));
pub(crate) static BUILD_LOCK_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| RetesterHomeDirectory::get_path().join("runtime-build.lock"));
