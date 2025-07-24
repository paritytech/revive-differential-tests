use std::{borrow::Cow, collections::HashSet, path::PathBuf};

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
}

impl FilesWithExtensionIterator {
    pub fn new(root_directory: PathBuf) -> Self {
        Self {
            allowed_extensions: Default::default(),
            directories_to_search: vec![root_directory],
            files_matching_allowed_extensions: Default::default(),
        }
    }

    pub fn with_allowed_extension(
        mut self,
        allowed_extension: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.allowed_extensions.insert(allowed_extension.into());
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

        // Read all of the entries in the directory. If we failed to read this dir's entires then we
        // elect to just ignore it and look in the next directory, we do that by calling the next
        // method again on the iterator, which is an intentional decision that we made here instead
        // of panicking.
        let Ok(dir_entries) = std::fs::read_dir(directory_to_search) else {
            return self.next();
        };

        for entry in dir_entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                self.directories_to_search.push(entry_path)
            } else if entry_path.is_file()
                && entry_path.extension().is_some_and(|ext| {
                    self.allowed_extensions
                        .iter()
                        .any(|allowed| ext.eq_ignore_ascii_case(allowed.as_ref()))
                })
            {
                self.files_matching_allowed_extensions.push(entry_path)
            }
        }

        self.next()
    }
}
