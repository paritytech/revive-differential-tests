use std::{borrow::Cow, collections::HashSet, path::PathBuf};

/// Defines wrappers around types.
///
/// For example, the macro invocation seen below:
///
/// ```rust,no_run
/// define_wrapper_type!(CaseId => usize);
/// ```
///
/// Would define a wrapper type that looks like the following:
///
/// ```rust,no_run
/// pub struct CaseId(usize);
/// ```
///
/// And would also implement a number of methods on this type making it easier
/// to use.
///
/// These wrapper types become very useful as they make the code a lot easier
/// to read.
///
/// Take the following as an example:
///
/// ```rust,no_run
/// struct State {
///     contracts: HashMap<usize, HashMap<String, Vec<u8>>>
/// }
/// ```
///
/// In the above code it's hard to understand what the various types refer to or
/// what to expect them to contain.
///
/// With these wrapper types we're able to create code that's self-documenting
/// in that the types tell us what the code is referring to. The above code is
/// transformed into
///
/// ```rust,no_run
/// struct State {
///     contracts: HashMap<CaseId, HashMap<ContractName, ContractByteCode>>
/// }
/// ```
#[macro_export]
macro_rules! define_wrapper_type {
    (
        $(#[$meta: meta])*
        $ident: ident => $ty: ty
    ) => {
        $(#[$meta])*
        pub struct $ident($ty);

        impl $ident {
            pub fn new(value: $ty) -> Self {
                Self(value)
            }

            pub fn into_inner(self) -> $ty {
                self.0
            }

            pub fn as_inner(&self) -> &$ty {
                &self.0
            }
        }

        impl AsRef<$ty> for $ident {
            fn as_ref(&self) -> &$ty {
                &self.0
            }
        }

        impl AsMut<$ty> for $ident {
            fn as_mut(&mut self) -> &mut $ty {
                &mut self.0
            }
        }

        impl std::ops::Deref for $ident {
            type Target = $ty;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::ops::DerefMut for $ident {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }
    };
}

define_wrapper_type!(
    /// Represents the ID of one of the cases in a metadata file.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    CaseId => usize
);

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
