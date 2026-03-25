use alloy::primitives::B256;

use crate::internal_prelude::*;

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
        let dummy_deployed_libraries = self.build_dummy_deployed_libraries();

        cached_compiler
            .compile_contracts(
                self.compilation_definition.metadata,
                self.compilation_definition.metadata_file_path,
                self.compilation_definition.mode.clone(),
                Some(&dummy_deployed_libraries),
                self.compilation_definition.compiler.as_ref(),
                self.compilation_definition.compiler_identifier,
                None,
                &CompilationReporter::PostLink(&self.compilation_definition.reporter),
            )
            .await
            .inspect_err(|err| error!(?err, "Compilation failed"))
            .context("Failed to produce the compiled contracts")?;

        Ok(())
    }

    /// Builds deterministic dummy library addresses from the declared libraries.
    /// Returns an empty map if there are no libraries declared (important in order to
    /// be treated as post-link throughout the code base).
    fn build_dummy_deployed_libraries(
        &self,
    ) -> HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)> {
        let mut dummy_deployed_libraries = HashMap::new();
        let Some(libraries) = self.compilation_definition.metadata.libraries.as_ref() else {
            return dummy_deployed_libraries;
        };

        // Addresses must be unique per library to mimic real deployments and avoid LLVM optimization divergence.
        let mut count: u64 = 1;
        for libraries_at_path in libraries.values() {
            for (library_ident, library_instance) in libraries_at_path {
                let library_address = Address::from_word(B256::from(U256::from(count)));
                dummy_deployed_libraries.insert(
                    library_instance.clone(),
                    (library_ident.clone(), library_address, JsonAbi::default()),
                );
                count += 1;
            }
        }

        dummy_deployed_libraries
    }
}
