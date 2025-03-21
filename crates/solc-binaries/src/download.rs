//! This module downloads solc binaries.

use std::{collections::HashMap, sync::Mutex};

use once_cell::sync::Lazy;

use crate::list::List;

pub static LIST_CACHE: Lazy<Mutex<HashMap<&'static str, List>>> = Lazy::new(Default::default);

impl List {
    pub const LINUX_URL: &str = "https://binaries.soliditylang.org/linux-amd64/list.json";
    pub const WINDOWS_URL: &str = "https://binaries.soliditylang.org/windows-amd64/list.json";
    pub const MACOSX_URL: &str = "https://binaries.soliditylang.org/macosx-amd64/list.json";
    pub const WASM_URL: &str = "https://binaries.soliditylang.org/wasm/list.json";

    /// Try to downloads the list from the given URL.
    ///
    /// Caches the list retrieved from the `url` into [LIST_CACHE],
    /// subsequent calls with the same `url` will return the cached list.
    pub fn download(url: &'static str) -> anyhow::Result<Self> {
        if let Some(list) = LIST_CACHE.lock().unwrap().get(url) {
            return Ok(list.clone());
        }

        let body: List = reqwest::blocking::get(url)?.json()?;

        LIST_CACHE.lock().unwrap().insert(url, body.clone());

        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use crate::list::List;

    #[test]
    fn try_get_windows_list() {
        List::download(List::WINDOWS_URL).unwrap();
        List::download(List::WINDOWS_URL).unwrap();
    }

    #[test]
    fn try_get_macosx_list() {
        List::download(List::MACOSX_URL).unwrap();
        List::download(List::MACOSX_URL).unwrap();
    }

    #[test]
    fn try_get_linux_list() {
        List::download(List::LINUX_URL).unwrap();
        List::download(List::LINUX_URL).unwrap();
    }

    #[test]
    fn try_get_wasm_list() {
        List::download(List::WASM_URL).unwrap();
        List::download(List::WASM_URL).unwrap();
    }
}
