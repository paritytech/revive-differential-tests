use revive_dt_config::CorpusConfiguration;
use revive_dt_format::{corpus::Corpus, metadata::MetadataFile};
use tracing::{info, info_span, instrument};

/// Given an object that implements [`AsRef<CorpusConfiguration>`], this function finds all of the
/// corpus files and produces a map containing all of the [`MetadataFile`]s discovered.
#[instrument(level = "debug", name = "Collecting Corpora", skip_all)]
pub fn collect_metadata_files(
    context: impl AsRef<CorpusConfiguration>,
) -> anyhow::Result<Vec<MetadataFile>> {
    let mut metadata_files = Vec::new();

    let corpus_configuration = AsRef::<CorpusConfiguration>::as_ref(&context);
    for path in &corpus_configuration.paths {
        let span = info_span!("Processing corpus file", path = %path.display());
        let _guard = span.enter();

        let corpus = Corpus::try_from_path(path)?;
        info!(
            name = corpus.name(),
            number_of_contained_paths = corpus.path_count(),
            "Deserialized corpus file"
        );
        metadata_files.extend(corpus.enumerate_tests());
    }

    // There's a possibility that there are certain paths that all lead to the same metadata files
    // and therefore it's important that we sort them and then deduplicate them.
    metadata_files.sort_by(|a, b| a.metadata_file_path.cmp(&b.metadata_file_path));
    metadata_files.dedup_by(|a, b| a.metadata_file_path == b.metadata_file_path);

    Ok(metadata_files)
}
