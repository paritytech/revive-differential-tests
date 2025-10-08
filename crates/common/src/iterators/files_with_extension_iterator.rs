use std::{
	borrow::Cow,
	collections::HashSet,
	path::{Path, PathBuf},
};

/// An iterator that finds files of a certain extension in the provided directory. You can think of
/// this a glob pattern similar to: `${path}/**/*.md`
pub struct FilesWithExtensionIterator {
	/// The set of allowed extensions that that match the requirement and that should be returned
	/// when found.
	allowed_extensions: HashSet<Cow<'static, str>>,

	/// The set of directories to visit next. This iterator does BFS and so these directories will
	/// only be visited if we can't find any files in our state.
	directories_to_search: Vec<PathBuf>,

	/// The set of files matching the allowed extensions that were found. If there are entries in
	/// this vector then they will be returned when the [`Iterator::next`] method is called. If not
	/// then we visit one of the next directories to visit.
	files_matching_allowed_extensions: Vec<PathBuf>,

	/// This option controls if the the cached file system should be used or not. This could be
	/// better for certain cases where the entries in the directories do not change and therefore
	/// caching can be used.
	use_cached_fs: bool,
}

impl FilesWithExtensionIterator {
	pub fn new(root_directory: impl AsRef<Path>) -> Self {
		Self {
			allowed_extensions: Default::default(),
			directories_to_search: vec![root_directory.as_ref().to_path_buf()],
			files_matching_allowed_extensions: Default::default(),
			use_cached_fs: Default::default(),
		}
	}

	pub fn with_allowed_extension(
		mut self,
		allowed_extension: impl Into<Cow<'static, str>>,
	) -> Self {
		self.allowed_extensions.insert(allowed_extension.into());
		self
	}

	pub fn with_use_cached_fs(mut self, use_cached_fs: bool) -> Self {
		self.use_cached_fs = use_cached_fs;
		self
	}
}

impl Iterator for FilesWithExtensionIterator {
	type Item = PathBuf;

	fn next(&mut self) -> Option<Self::Item> {
		if let Some(file_path) = self.files_matching_allowed_extensions.pop() {
			return Some(file_path);
		};

		let directory_to_search = self.directories_to_search.pop()?;

		let iterator = if self.use_cached_fs {
			let Ok(dir_entries) = crate::cached_fs::read_dir(directory_to_search.as_path()) else {
				return self.next();
			};
			Box::new(dir_entries) as Box<dyn Iterator<Item = std::io::Result<PathBuf>>>
		} else {
			let Ok(dir_entries) = std::fs::read_dir(directory_to_search) else {
				return self.next();
			};
			Box::new(dir_entries.map(|maybe_entry| maybe_entry.map(|entry| entry.path()))) as Box<_>
		};

		for entry_path in iterator.flatten() {
			if entry_path.is_dir() {
				self.directories_to_search.push(entry_path)
			} else if entry_path.is_file() &&
				entry_path.extension().is_some_and(|ext| {
					self.allowed_extensions
						.iter()
						.any(|allowed| ext.eq_ignore_ascii_case(allowed.as_ref()))
				}) {
				self.files_matching_allowed_extensions.push(entry_path)
			}
		}

		self.next()
	}
}
