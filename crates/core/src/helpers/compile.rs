use std::sync::Arc;
use std::{borrow::Cow, path::Path};

use futures::{Stream, StreamExt, stream};
use indexmap::{IndexMap, indexmap};
use revive_dt_common::types::CompilerIdentifier;
use revive_dt_compiler::revive_resolc::Resolc;
use revive_dt_config::Context;
use revive_dt_format::corpus::Corpus;
use serde_json::{Value, json};

use revive_dt_compiler::Mode;
use revive_dt_compiler::SolidityCompiler;
use revive_dt_format::metadata::MetadataFile;
use revive_dt_report::{CompilationSpecifier, Reporter, StandaloneCompilationSpecificReporter};
use tracing::{debug, error, info};

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
                    reporter.compilation_specific_reporter(Arc::new(CompilationSpecifier {
                        solc_mode: mode.clone(),
                        metadata_file_path: metadata_file.metadata_file_path.clone(),
                    })),
                )
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
}

type CompilationCheckFunctionResult = Result<(), (&'static str, IndexMap<&'static str, Value>)>;
