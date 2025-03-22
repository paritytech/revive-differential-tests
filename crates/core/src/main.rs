use std::collections::BTreeSet;

use rayon::prelude::*;

use revive_dt_config::*;
use revive_dt_core::driver::compiler::build_evm;
use revive_dt_format::corpus::Corpus;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    for path in get_args().corpus.iter().collect::<BTreeSet<_>>() {
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
