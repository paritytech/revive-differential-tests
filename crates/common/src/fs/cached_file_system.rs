//! Implements a cached file system that allows for files to be read once into memory and then when
//! they're requested to be read again they will be returned from the cache.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use anyhow::Result;
use tokio::sync::RwLock;

#[allow(clippy::type_complexity)]
static CACHE: LazyLock<Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>> = LazyLock::new(Default::default);

pub struct CachedFileSystem;

impl CachedFileSystem {
    pub async fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let cache_read_lock = CACHE.read().await;
        match cache_read_lock.get(path.as_ref()) {
            Some(entry) => Ok(entry.clone()),
            None => {
                drop(cache_read_lock);

                let content = std::fs::read(&path)?;
                let mut cache_write_lock = CACHE.write().await;
                cache_write_lock.insert(path.as_ref().to_path_buf(), content.clone());
                Ok(content)
            }
        }
    }

    pub async fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
        let content = Self::read(path).await?;
        String::from_utf8(content).map_err(Into::into)
    }
}
