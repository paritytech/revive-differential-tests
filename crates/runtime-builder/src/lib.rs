mod build;
mod checkout;
mod paths;

pub mod prelude {
    pub use crate::build::{BuiltRuntime, build_runtime};
}

pub(crate) mod internal_prelude {
    pub(crate) use crate::{checkout::checkout, paths::*};
    pub use revive_dt_common::prelude::*;

    pub use std::{path::PathBuf, process::Stdio, sync::LazyLock};

    pub use anyhow::{Context as _, Result, ensure};
    pub use git2::{
        AutotagOption, FetchOptions, Oid, Repository,
        build::{CheckoutBuilder, RepoBuilder},
    };
    pub use tokio::{
        fs::{self, File},
        process::Command,
        task::spawn_blocking,
    };
    pub use tracing::{debug, info};
}
