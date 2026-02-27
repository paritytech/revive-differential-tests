use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
};

use itertools::Itertools;
use revive_dt_common::{
    iterators::{EitherIter, FilesWithExtensionIterator},
    types::{Mode, ParsedCompilationSpecifier, ParsedMode, ParsedTestSpecifier},
};
use tracing::{debug, warn};

use crate::{
    case::{Case, CaseIdx},
    metadata::{Metadata, MetadataFile},
};

#[derive(Default)]
pub struct Corpus {
    test_specifiers: HashMap<ParsedTestSpecifier, Vec<PathBuf>>,
    compilation_specifiers: HashMap<ParsedCompilationSpecifier, Vec<PathBuf>>,
    metadata_files: HashMap<PathBuf, MetadataFile>,
}

impl Corpus {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_test_specifier(
        mut self,
        test_specifier: ParsedTestSpecifier,
    ) -> anyhow::Result<Self> {
        match &test_specifier {
            ParsedTestSpecifier::FileOrDirectory {
                metadata_or_directory_file_path: metadata_file_path,
            }
            | ParsedTestSpecifier::Case {
                metadata_file_path, ..
            }
            | ParsedTestSpecifier::CaseWithMode {
                metadata_file_path, ..
            } => {
                let metadata_files = enumerate_metadata_files(metadata_file_path);
                self.test_specifiers.insert(
                    test_specifier,
                    metadata_files
                        .iter()
                        .map(|metadata_file| metadata_file.metadata_file_path.clone())
                        .collect(),
                );
                for metadata_file in metadata_files.into_iter() {
                    self.metadata_files
                        .insert(metadata_file.metadata_file_path.clone(), metadata_file);
                }
            }
        };

        Ok(self)
    }

    pub fn with_compilation_specifier(
        mut self,
        compilation_specifier: ParsedCompilationSpecifier,
    ) -> anyhow::Result<Self> {
        match &compilation_specifier {
            ParsedCompilationSpecifier::FileOrDirectory {
                metadata_or_directory_file_path: metadata_file_path,
            } => {
                let metadata_files = enumerate_metadata_files(metadata_file_path);
                self.compilation_specifiers.insert(
                    compilation_specifier,
                    metadata_files
                        .iter()
                        .map(|metadata_file| metadata_file.metadata_file_path.clone())
                        .collect(),
                );
                for metadata_file in metadata_files.into_iter() {
                    self.metadata_files
                        .insert(metadata_file.metadata_file_path.clone(), metadata_file);
                }
            }
        };

        Ok(self)
    }

    pub fn cases_iterator(
        &self,
    ) -> impl Iterator<Item = (&'_ MetadataFile, CaseIdx, &'_ Case, Cow<'_, Mode>)> + '_ {
        let mut iterator = Box::new(std::iter::empty())
            as Box<dyn Iterator<Item = (&'_ MetadataFile, CaseIdx, &'_ Case, Cow<'_, Mode>)> + '_>;

        for (test_specifier, metadata_file_paths) in self.test_specifiers.iter() {
            for metadata_file_path in metadata_file_paths {
                let metadata_file = self
                    .metadata_files
                    .get(metadata_file_path)
                    .expect("Must succeed");

                match test_specifier {
                    ParsedTestSpecifier::FileOrDirectory { .. } => {
                        for (case_idx, case) in metadata_file.cases.iter().enumerate() {
                            let case_idx = CaseIdx::new(case_idx);

                            let modes = case.modes.as_ref().or(metadata_file.modes.as_ref());
                            let modes = match modes {
                                Some(modes) => EitherIter::A(
                                    ParsedMode::many_to_modes(modes.iter())
                                        .map(Cow::<'static, _>::Owned),
                                ),
                                None => EitherIter::B(Mode::all().map(Cow::<'static, _>::Borrowed)),
                            };

                            iterator = Box::new(
                                iterator.chain(
                                    modes
                                        .into_iter()
                                        .map(move |mode| (metadata_file, case_idx, case, mode)),
                                ),
                            )
                        }
                    }
                    ParsedTestSpecifier::Case { case_idx, .. } => {
                        let Some(case) = metadata_file.cases.get(*case_idx) else {
                            warn!(
                                test_specifier = %test_specifier,
                                metadata_file_path = %metadata_file_path.display(),
                                case_idx = case_idx,
                                case_count = metadata_file.cases.len(),
                                "Specified case not found in metadata file"
                            );
                            continue;
                        };
                        let case_idx = CaseIdx::new(*case_idx);

                        let modes = case.modes.as_ref().or(metadata_file.modes.as_ref());
                        let modes = match modes {
                            Some(modes) => EitherIter::A(
                                ParsedMode::many_to_modes(modes.iter())
                                    .map(Cow::<'static, Mode>::Owned),
                            ),
                            None => EitherIter::B(Mode::all().map(Cow::<'static, _>::Borrowed)),
                        };

                        iterator = Box::new(
                            iterator.chain(
                                modes
                                    .into_iter()
                                    .map(move |mode| (metadata_file, case_idx, case, mode)),
                            ),
                        )
                    }
                    ParsedTestSpecifier::CaseWithMode { case_idx, mode, .. } => {
                        let Some(case) = metadata_file.cases.get(*case_idx) else {
                            warn!(
                                test_specifier = %test_specifier,
                                metadata_file_path = %metadata_file_path.display(),
                                case_idx = case_idx,
                                case_count = metadata_file.cases.len(),
                                "Specified case not found in metadata file"
                            );
                            continue;
                        };
                        let case_idx = CaseIdx::new(*case_idx);

                        let mode = Cow::Borrowed(mode);
                        iterator = Box::new(iterator.chain(std::iter::once((
                            metadata_file,
                            case_idx,
                            case,
                            mode,
                        ))))
                    }
                }
            }
        }

        iterator.unique_by(|item| (&item.0.metadata_file_path, item.1, item.3.clone()))
    }

    /// Iterator over the metadata files for the compilation specifiers.
    pub fn compilation_metadata_files_iterator(
        &self,
    ) -> impl Iterator<Item = &'_ MetadataFile> + '_ {
        self.compilation_specifiers
            .values()
            .flatten()
            .map(|path| self.metadata_files.get(path).expect("Must succeed"))
            .unique_by(|metadata_file| &metadata_file.metadata_file_path)
    }

    pub fn metadata_file_count(&self) -> usize {
        self.metadata_files.len()
    }
}

fn enumerate_metadata_files(path: impl AsRef<Path>) -> Vec<MetadataFile> {
    let root_path = path.as_ref();
    let mut tests = if !root_path.is_dir() {
        Box::new(std::iter::once(root_path.to_path_buf())) as Box<dyn Iterator<Item = _>>
    } else {
        Box::new(
            FilesWithExtensionIterator::new(root_path)
                .with_use_cached_fs(true)
                .with_allowed_extension("sol")
                .with_allowed_extension("json"),
        )
    }
    .map(move |metadata_file_path| (root_path, metadata_file_path))
    .filter_map(|(root_path, metadata_file_path)| {
        Metadata::try_from_file(&metadata_file_path)
            .or_else(|| {
                debug!(
                    discovered_from = %root_path.display(),
                    metadata_file_path = %metadata_file_path.display(),
                    "Skipping file since it doesn't contain valid metadata"
                );
                None
            })
            .map(|metadata| MetadataFile {
                metadata_file_path,
                corpus_file_path: root_path.to_path_buf(),
                content: metadata,
            })
            .inspect(|metadata_file| {
                debug!(
                    metadata_file_path = %metadata_file.relative_path().display(),
                    "Loaded metadata file"
                )
            })
    })
    .collect::<Vec<_>>();
    tests.sort_by(|a, b| a.metadata_file_path.cmp(&b.metadata_file_path));
    tests.dedup_by(|a, b| a.metadata_file_path == b.metadata_file_path);
    tests
}
