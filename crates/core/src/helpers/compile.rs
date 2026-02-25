use std::sync::{Arc, LazyLock};
use std::{borrow::Cow, path::Path};

use futures::{Stream, StreamExt, stream};
use indexmap::{IndexMap, indexmap};
use regex::Regex;
use revive_dt_common::{cached_fs::read_to_string, types::CompilerIdentifier};
use revive_dt_compiler::{Mode, SolidityCompiler, revive_resolc::Resolc};
use revive_dt_config::Context;
use revive_dt_format::{corpus::Corpus, metadata::MetadataFile};
use revive_dt_report::{
    Reporter, StandaloneCompilationSpecificReporter, StandaloneCompilationSpecifier,
};
use semver::VersionReq;
use serde_json::{self, json};
use tracing::{debug, error, info};

/// This is a full description of a compilation to run alongside the full metadata file
/// and the specific mode to compile with.
pub struct CompilationDefinition<'a> {
    pub metadata: &'a MetadataFile,
    pub metadata_file_path: &'a Path,
    pub mode: Cow<'a, Mode>,
    pub compiler_identifier: CompilerIdentifier,
    pub compiler: Box<dyn SolidityCompiler + 'static>,
    pub reporter: StandaloneCompilationSpecificReporter,
}

impl<'a> CompilationDefinition<'a> {
    /// Checks if this compilation can be run with the current configuration.
    pub fn check_compatibility(&self) -> CompilationCheckFunctionResult {
        self.check_metadata_file_ignored()?;
        self.check_compiler_compatibility()?;
        self.check_pragma_solidity_compatibility()?;
        Ok(())
    }

    /// Checks if the metadata file is ignored or not.
    fn check_metadata_file_ignored(&self) -> CompilationCheckFunctionResult {
        if self.metadata.ignore.is_some_and(|ignore| ignore) {
            Err(("Metadata file is ignored.", indexmap! {}))
        } else {
            Ok(())
        }
    }

    /// Checks if the compiler supports the provided mode.
    fn check_compiler_compatibility(&self) -> CompilationCheckFunctionResult {
        let mut error_map = indexmap! {};
        let is_compatible = self
            .compiler
            .supports_mode(self.mode.optimize_setting, self.mode.pipeline);
        error_map.insert(self.compiler_identifier.into(), json!(is_compatible));

        if is_compatible {
            Ok(())
        } else {
            Err(("The compiler does not support this mode.", error_map))
        }
    }

    /// Checks if the file-specified Solidity version is compatible with the configured version.
    fn check_pragma_solidity_compatibility(&self) -> CompilationCheckFunctionResult {
        let files_to_compile = self.metadata.files_to_compile().map_err(|e| {
            (
                "Failed to enumerate files to compile.",
                indexmap! {
                    "metadata_file_path" => json!(self.metadata_file_path.display().to_string()),
                    "error" => json!(e.to_string()),
                },
            )
        })?;
        let mut incompatible_files: Vec<serde_json::Value> = Vec::new();

        for source_path in files_to_compile {
            let source = read_to_string(&source_path).map_err(|e| {
                (
                    "Failed to read source file.",
                    indexmap! {
                        "source_path" => json!(source_path.display().to_string()),
                        "error" => json!(e.to_string()),
                    },
                )
            })?;

            if let Some(version_requirement) = Self::parse_pragma_solidity_requirement(&source) {
                if !version_requirement.matches(self.compiler.version()) {
                    incompatible_files.push(json!({
                        "source_path": source_path.display().to_string(),
                        "pragma": version_requirement.to_string(),
                    }));
                }
            }
        }

        if incompatible_files.is_empty() {
            Ok(())
        } else {
            Err((
                "Source pragma is incompatible with the Solidity compiler version.",
                indexmap! {
                    "compiler_version" => json!(self.compiler.version().to_string()),
                    "incompatible_files" => json!(incompatible_files),
                },
            ))
        }
    }

    /// Parses the Solidity version requirement from `source`.
    /// Returns `None` if no pragma is found or if it cannot be parsed.
    fn parse_pragma_solidity_requirement(source: &str) -> Option<VersionReq> {
        static PRAGMA_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"pragma\s+solidity\s+(?P<version>[^;]+);").unwrap());

        let caps = PRAGMA_REGEX.captures(source)?;
        let solidity_version_format = caps.name("version")?.as_str().trim();
        let semver_format = Self::solidity_version_to_semver(solidity_version_format);

        VersionReq::parse(&semver_format).ok()
    }

    /// Converts Solidity version constraints to semver-compatible format.
    /// Example:
    /// ```txt
    /// Solidity: ">=0.8.0 <0.9.0" or "^0.8.0" or "0.8.33"
    /// semver:   ">=0.8.0, <0.9.0" or "^0.8.0" or "=0.8.33"
    /// ```
    fn solidity_version_to_semver(version: &str) -> String {
        version
            .split_whitespace()
            .map(|part| {
                let is_exact_version = part.starts_with(|c: char| c.is_ascii_digit());
                if is_exact_version {
                    format!("={}", part)
                } else {
                    part.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

type CompilationCheckFunctionResult =
    Result<(), (&'static str, IndexMap<&'static str, serde_json::Value>)>;

/// Creates a stream of [`CompilationDefinition`]s for the contracts to be compiled.
pub async fn create_compilation_definitions_stream<'a>(
    context: &Context,
    corpus: &'a Corpus,
    mode: Mode,
    reporter: Reporter,
) -> impl Stream<Item = CompilationDefinition<'a>> {
    let cloned_reporter = reporter.clone();
    stream::iter(
        corpus
            .compilation_metadata_files_iterator()
            .inspect(move |metadata_file| {
                cloned_reporter
                    .report_metadata_file_discovery_event(
                        metadata_file.metadata_file_path.clone(),
                        metadata_file.content.clone(),
                    )
                    .unwrap();
            })
            .map(move |metadata_file| {
                let reporter = reporter.clone();

                (
                    metadata_file,
                    Cow::<'_, Mode>::Owned(mode.clone()),
                    reporter.compilation_specific_reporter(Arc::new(
                        StandaloneCompilationSpecifier {
                            solc_mode: mode.clone(),
                            metadata_file_path: metadata_file.metadata_file_path.clone(),
                        },
                    )),
                )
            })
            .inspect(|(_, _, reporter)| {
                reporter
                    .report_standalone_compilation_discovery_event()
                    .expect("Can't fail");
            }),
    )
    // Creating the `CompilationDefinition` objects from all of the various objects we have.
    .filter_map(move |(metadata_file, mode, reporter)| async move {
        // NOTE: Currently always specifying the resolc compiler.
        let compiler = Resolc::new(context.clone(), mode.version.clone().map(Into::into))
            .await
            .map(|compiler| Box::new(compiler) as Box<dyn SolidityCompiler>)
            .inspect_err(|err| error!(?err, "Failed to instantiate the compiler"))
            .ok()?;

        Some(CompilationDefinition {
            metadata: metadata_file,
            metadata_file_path: metadata_file.metadata_file_path.as_path(),
            mode: mode.clone(),
            // NOTE: Currently always specifying the resolc compiler.
            compiler_identifier: CompilerIdentifier::Resolc,
            compiler,
            reporter,
        })
    })
    // Filter out the compilations which are incompatible.
    .filter_map(move |compilation| async move {
        match compilation.check_compatibility() {
            Ok(()) => Some(compilation),
            Err((reason, additional_information)) => {
                debug!(
                    metadata_file_path = %compilation.metadata.metadata_file_path.display(),
                    mode = %compilation.mode,
                    reason,
                    additional_information =
                        serde_json::to_string(&additional_information).unwrap(),
                    "Ignoring Compilation"
                );
                compilation
                    .reporter
                    .report_standalone_contracts_compilation_ignored_event(
                        reason.to_string(),
                        additional_information
                            .into_iter()
                            .map(|(k, v)| (k.into(), v))
                            .collect::<IndexMap<_, _>>(),
                    )
                    .expect("Can't fail");
                None
            }
        }
    })
    .inspect(|compilation| {
        info!(
            metadata_file_path = %compilation.metadata_file_path.display(),
            mode = %compilation.mode,
            "Created a compilation definition"
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

    #[test]
    fn test_parse_pragma_compound_constraint() {
        let source = r#"
            // SPDX-License-Identifier: MIT
            pragma solidity >=0.8.0 <0.9.0;

            contract Test {}
        "#;
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source).unwrap();
        assert_eq!(req, VersionReq::parse(">=0.8.0, <0.9.0").unwrap());
        assert!(req.matches(&Version::new(0, 8, 0)));
        assert!(req.matches(&Version::new(0, 8, 99)));
        assert!(!req.matches(&Version::new(0, 7, 99)));
        assert!(!req.matches(&Version::new(0, 9, 0)));
    }

    #[test]
    fn test_parse_pragma_exact_version() {
        let source = r#"
            // SPDX-License-Identifier: MIT
            pragma   solidity   0.8.19;

            contract Test {}
        "#;
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source).unwrap();
        assert_eq!(req, VersionReq::parse("=0.8.19").unwrap());
        assert!(req.matches(&Version::new(0, 8, 19)));
        assert!(!req.matches(&Version::new(0, 8, 20)));
    }

    #[test]
    fn test_parse_pragma_caret_version() {
        let source = "pragma solidity ^0.8.0;";
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source).unwrap();
        assert_eq!(req, VersionReq::parse("^0.8.0").unwrap());
        assert!(req.matches(&Version::new(0, 8, 0)));
        assert!(req.matches(&Version::new(0, 8, 33)));
        assert!(!req.matches(&Version::new(0, 9, 0)));
        assert!(!req.matches(&Version::new(0, 7, 0)));
    }

    #[test]
    fn test_parse_pragma_tilde_version() {
        let source = "pragma solidity ~0.8.19;";
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source).unwrap();
        assert_eq!(req, VersionReq::parse("~0.8.19").unwrap());
        assert!(req.matches(&Version::new(0, 8, 19)));
        assert!(req.matches(&Version::new(0, 8, 33)));
        assert!(!req.matches(&Version::new(0, 8, 18)));
        assert!(!req.matches(&Version::new(0, 9, 0)));
    }

    #[test]
    fn test_parse_pragma_upper_bound_version() {
        let source = "pragma solidity <=0.4.21;";
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source).unwrap();
        assert_eq!(req, VersionReq::parse("<=0.4.21").unwrap());
        assert!(req.matches(&Version::new(0, 4, 21)));
        assert!(req.matches(&Version::new(0, 4, 20)));
        assert!(!req.matches(&Version::new(0, 8, 33)));
    }

    #[test]
    fn test_parse_pragma_missing() {
        let source = r#"
            // SPDX-License-Identifier: MIT
            contract Test {}
        "#;
        let req = CompilationDefinition::parse_pragma_solidity_requirement(source);
        assert!(req.is_none());
    }
}
