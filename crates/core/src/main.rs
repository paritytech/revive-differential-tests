use std::collections::BTreeSet;

use clap::Parser;
use rayon::prelude::*;

use revive_dt_config::*;
use revive_dt_core::driver::compiler::build_evm;
use revive_dt_format::corpus::Corpus;
use temp_dir::TempDir;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut config = Arguments::parse();

    if config.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }

    let temporary_directory = TempDir::new()?;
    config
        .working_directory
        .get_or_insert_with(|| temporary_directory.path().into());

    for path in config.corpus.iter().collect::<BTreeSet<_>>() {
        log::trace!("attempting corpus {path:?}");
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {corpus:?}");

        let tests = corpus.enumerate_tests();
        log::info!("found {} tests", tests.len());

        tests
            .par_iter()
            .for_each(|metadata| match build_evm(metadata) {
                Ok(_) => log::info!(
                    "metadata {} compilation success",
                    metadata.path.as_ref().unwrap().display()
                ),
                Err(error) => log::warn!(
                    "metadata {} compilation failure: {error:?}",
                    metadata.path.as_ref().unwrap().display()
                ),
            });
    }

    Ok(())
}
