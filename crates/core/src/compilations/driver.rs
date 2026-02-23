use anyhow::{Context as _, Result};
use revive_dt_report::CompilationReporter;
use tracing::error;

use crate::helpers::{CachedCompiler, CompilationDefinition};

/// The compilation driver.
pub struct Driver<'a> {
    /// The definition of the compilation that the driver is instructed to execute.
    compilation_definition: &'a CompilationDefinition<'a>,
}

impl<'a> Driver<'a> {
    /// Creates a new driver.
    pub fn new(compilation_definition: &'a CompilationDefinition<'a>) -> Self {
        Self {
            compilation_definition,
        }
    }

    /// Compiles all contracts specified by the [`CompilationDefinition`].
    pub async fn compile_all(&self, cached_compiler: &CachedCompiler<'a>) -> Result<()> {
        cached_compiler
            .compile_contracts(
                self.compilation_definition.metadata,
                self.compilation_definition.metadata_file_path,
                self.compilation_definition.mode.clone(),
                None,
                self.compilation_definition.compiler.as_ref(),
                self.compilation_definition.compiler_identifier,
                None,
                &CompilationReporter::Standalone(&self.compilation_definition.reporter),
            )
            .await
            .inspect_err(|err| error!(?err, "Compilation failed"))
            .context("Failed to produce the compiled contracts")?;

        Ok(())
    }
}
