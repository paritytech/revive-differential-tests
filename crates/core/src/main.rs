use std::collections::BTreeSet;

use clap::Parser;

use revive_dt_core::arguments::Arguments;
use revive_dt_format::corpus::Corpus;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let args = Arguments::try_parse()?;

    for path in args.corpus.iter().collect::<BTreeSet<_>>() {
        log::trace!("attempting corpus {path:?}");
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {corpus:?}");

        let tests = corpus.enumerate_tests();

        log::debug!("{tests:?}");
    }

    Ok(())
}
