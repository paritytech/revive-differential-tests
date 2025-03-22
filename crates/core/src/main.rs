use std::collections::BTreeSet;

use clap::Parser;

use rayon::prelude::*;
use revive_dt_core::{arguments::Arguments, driver::compiler::build_evm};
use revive_dt_format::corpus::Corpus;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let args = Arguments::try_parse()?;

    for path in args.corpus.iter().collect::<BTreeSet<_>>() {
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
