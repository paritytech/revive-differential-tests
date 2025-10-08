//! This module implements a cached file system allowing for results to be stored in-memory rather
//! rather being queried from the file system again.

use std::{
	fs,
	io::{Error, Result},
	path::{Path, PathBuf},
};

use moka::sync::Cache;
use once_cell::sync::Lazy;

pub fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
	static READ_CACHE: Lazy<Cache<PathBuf, Vec<u8>>> = Lazy::new(|| Cache::new(10_000));

	let path = path.as_ref().canonicalize()?;
	match READ_CACHE.get(path.as_path()) {
		Some(content) => Ok(content),
		None => {
			let content = fs::read(path.as_path())?;
			READ_CACHE.insert(path, content.clone());
			Ok(content)
		},
	}
}

pub fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
	let content = read(path)?;
	String::from_utf8(content).map_err(|_| {
		Error::new(std::io::ErrorKind::InvalidData, "The contents of the file are not valid UTF8")
	})
}

pub fn read_dir(path: impl AsRef<Path>) -> Result<Box<dyn Iterator<Item = Result<PathBuf>>>> {
	static READ_DIR_CACHE: Lazy<Cache<PathBuf, Vec<PathBuf>>> = Lazy::new(|| Cache::new(10_000));

	let path = path.as_ref().canonicalize()?;
	match READ_DIR_CACHE.get(path.as_path()) {
		Some(entries) => Ok(Box::new(entries.into_iter().map(Ok)) as Box<_>),
		None => {
			let entries = fs::read_dir(path.as_path())?
				.flat_map(|maybe_entry| maybe_entry.map(|entry| entry.path()))
				.collect();
			READ_DIR_CACHE.insert(path.clone(), entries);
			Ok(read_dir(path).unwrap())
		},
	}
}
